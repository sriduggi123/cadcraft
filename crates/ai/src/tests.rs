use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use cadcraft_collab::message::{ClientMsg, ServerMsg};
use cadcraft_collab::{Json, Link};
use cadcraft_engine::Session;
use serde_json::{Value, json};

use crate::{Assistant, Entry, tools};

/// Messages between the assistant and a pretend server.
#[derive(Default)]
struct Wire {
    up: VecDeque<ClientMsg>,
    down: VecDeque<ServerMsg>,
}

struct MemLink(Arc<Mutex<Wire>>);

impl Link for MemLink {
    fn send(&mut self, msg: &ClientMsg) -> cadcraft_collab::Result<()> {
        self.0.lock().unwrap().up.push_back(msg.clone());
        Ok(())
    }
    fn recv(&mut self) -> Option<cadcraft_collab::Result<ServerMsg>> {
        self.0.lock().unwrap().down.pop_front().map(Ok)
    }
    fn is_open(&self) -> bool {
        true
    }
}

fn setup() -> (Session, Assistant, Arc<Mutex<Wire>>) {
    let mut s = Session::new();
    s.execute("new", &json!({})).unwrap();
    let wire = Arc::new(Mutex::new(Wire::default()));
    let mut a = Assistant::new("ws://127.0.0.1:1");
    a.attach(Box::new(MemLink(wire.clone())));
    (s, a, wire)
}

/// The last request the assistant sent: its id and body.
fn last_request(wire: &Arc<Mutex<Wire>>) -> (u64, Value) {
    match wire.lock().unwrap().up.pop_back() {
        Some(ClientMsg::Ai { id, request, .. }) => (id, request.0),
        other => panic!("expected an AI request, got {other:?}"),
    }
}

fn answer(wire: &Arc<Mutex<Wire>>, id: u64, message: Value) {
    let reply = json!({"choices": [{"index": 0, "message": message, "finish_reason": "stop"}]});
    wire.lock().unwrap().down.push_back(ServerMsg::Ai { id, reply: Ok(Json(reply)) });
}

fn call(id: &str, name: &str, args: Value) -> Value {
    json!({"id": id, "type": "function", "function": {"name": name, "arguments": args.to_string()}})
}

fn circles(s: &Session) -> usize {
    s.state().unwrap().doc.model.iter().filter(|e| matches!(e.kind, cadcraft_engine::doc::EntityKind::Circle(_))).count()
}

#[test]
fn a_request_runs_the_tools_the_model_asks_for_and_is_one_undo_step() {
    let (mut s, mut a, wire) = setup();
    a.ask(&mut s, "draw two circles");
    let (id, req) = last_request(&wire);
    assert_eq!(req["messages"][0]["role"], "system");
    assert!(req["messages"][1]["content"].as_str().unwrap().contains("[The drawing now:"));
    assert!(req["tools"].as_array().unwrap().iter().any(|t| t["function"]["name"] == "execute"));

    let draw = |x: f64| call(&format!("c{x}"), "execute", json!({"command": "circle", "params": {"center": [x, 0], "radius": 2}}));
    answer(&wire, id, json!({"role": "assistant", "content": "Drawing them.", "tool_calls": [draw(0.0), draw(10.0)]}));
    assert!(a.tick(&mut s));
    assert_eq!(circles(&s), 2);
    // The results go back with the calls' ids.
    let (id, req) = last_request(&wire);
    let msgs = req["messages"].as_array().unwrap();
    assert_eq!(msgs[msgs.len() - 2]["tool_call_id"], "c0");
    assert_eq!(msgs[msgs.len() - 1]["role"], "tool");

    answer(&wire, id, json!({"role": "assistant", "content": "Two circles of radius 2 at 0,0 and 10,0."}));
    assert!(a.tick(&mut s));
    assert!(!a.busy());
    assert_eq!(
        a.log,
        vec![
            Entry::User("draw two circles".into()),
            Entry::Assistant("Drawing them.".into()),
            Entry::Tool { line: "Ran CIRCLE".into(), ok: true },
            Entry::Tool { line: "Ran CIRCLE".into(), ok: true },
            Entry::Assistant("Two circles of radius 2 at 0,0 and 10,0.".into()),
        ]
    );
    let st = s.state().unwrap();
    assert_eq!(st.undo.len(), 1, "one undo step for the request");
    assert_eq!(st.undo[0].label, "AI: draw two circles");
    s.execute("u", &json!({})).unwrap();
    assert_eq!(circles(&s), 0);
}

#[test]
fn a_failing_call_goes_back_to_the_model_as_an_error() {
    let (mut s, mut a, wire) = setup();
    a.ask(&mut s, "do something");
    let (id, _) = last_request(&wire);
    answer(&wire, id, json!({"role": "assistant", "content": "", "tool_calls": [call("x", "execute", json!({"command": "no-such-command"}))]}));
    a.tick(&mut s);
    let (_, req) = last_request(&wire);
    let last = req["messages"].as_array().unwrap().last().unwrap().clone();
    assert!(last["content"].as_str().unwrap().starts_with("error:"), "{last}");
    assert!(matches!(a.log.last(), Some(Entry::Tool { ok: false, .. })));
    assert!(a.busy());
}

#[test]
fn a_refusal_from_the_server_ends_the_request() {
    let (mut s, mut a, wire) = setup();
    a.ask(&mut s, "hello");
    let (id, _) = last_request(&wire);
    wire.lock().unwrap().down.push_back(ServerMsg::Ai { id, reply: Err("this server has no AI set up".into()) });
    a.tick(&mut s);
    assert!(!a.busy());
    assert_eq!(a.log.last(), Some(&Entry::Error("this server has no AI set up".into())));
    // A new request can follow.
    a.ask(&mut s, "again");
    assert!(a.busy());
}

#[test]
fn stop_keeps_the_changes_as_one_undo_step() {
    let (mut s, mut a, wire) = setup();
    a.ask(&mut s, "circles");
    let (id, _) = last_request(&wire);
    let c = call("a", "execute", json!({"command": "circle", "params": {"center": [0, 0], "radius": 1}}));
    answer(&wire, id, json!({"role": "assistant", "content": "", "tool_calls": [c]}));
    a.tick(&mut s);
    a.stop(&mut s);
    assert!(!a.busy());
    assert_eq!(circles(&s), 1);
    assert_eq!(s.state().unwrap().undo.last().unwrap().label, "AI: circles");
}

#[test]
fn find_commands_puts_the_named_command_first() {
    let s = Session::new();
    let found = tools::find_commands(&s, "circle");
    assert_eq!(found[0]["id"], "circle");
    assert!(found[0]["params"].as_str().unwrap().contains("radius"));
    assert!(found.as_array().unwrap().len() <= 20);
    assert!(tools::find_commands(&s, "zzzz-nothing").as_array().unwrap().is_empty());
}

#[test]
fn tools_read_the_drawing_and_refuse_unknown_names() {
    let (mut s, _, _) = setup();
    s.execute("circle", &json!({"center": [5, 5], "radius": 3})).unwrap();
    let out = tools::run(&mut s, "query_entities", &json!({"type": "Circle"}));
    assert!(out.ok, "{}", out.result);
    assert!(out.result.contains("\"count\":1"), "{}", out.result);
    let out = tools::run(&mut s, "drawing_summary", &json!({}));
    assert!(out.ok && out.result.contains("counts"), "{}", out.result);
    let out = tools::run(&mut s, "command_line", &json!({"text": "circle 20,0 1"}));
    assert!(out.ok, "{}", out.result);
    assert_eq!(circles(&s), 2);
    let out = tools::run(&mut s, "format_disk", &json!({}));
    assert!(!out.ok);
}

#[test]
fn every_tool_definition_names_a_tool_that_runs() {
    let (mut s, _, _) = setup();
    for t in tools::definitions().as_array().unwrap() {
        let name = t["function"]["name"].as_str().unwrap();
        let out = tools::run(&mut s, name, &json!({"query": "line", "text": ""}));
        assert!(!out.result.starts_with("error: there is no tool"), "{name}");
    }
}
