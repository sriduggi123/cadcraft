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
        if tools::USER_TOOLS.contains(&name) {
            continue;
        }
        let out = tools::run(&mut s, name, &json!({"query": "line", "text": ""}));
        assert!(!out.result.starts_with("error: there is no tool"), "{name}");
    }
}

use crate::{Answer, Need, parse_points};

/// The model asks for one call; returns the request id that comes next.
fn model_calls(a: &mut Assistant, s: &mut Session, wire: &Arc<Mutex<Wire>>, name: &str, args: Value) {
    let (id, _) = last_request(wire);
    answer(wire, id, json!({"role": "assistant", "content": "", "tool_calls": [call("t1", name, args)]}));
    a.tick(s);
}

/// The last tool result sent to the model.
fn last_result(wire: &Arc<Mutex<Wire>>) -> String {
    let (_, req) = last_request(wire);
    let msgs = req["messages"].as_array().unwrap();
    let tool = msgs.iter().rev().find(|m| m["role"] == "tool").unwrap();
    tool["content"].as_str().unwrap().to_string()
}

#[test]
fn opening_a_file_waits_for_a_yes_and_a_no_goes_back_to_the_model() {
    let (mut s, mut a, wire) = setup();
    a.ask(&mut s, "open my other drawing");
    model_calls(&mut a, &mut s, &wire, "execute", json!({"command": "open", "params": {"path": "/tmp/x.dxf"}}));
    assert!(matches!(a.need(), Some(Need::Confirm { lisp: false, .. })), "{:?}", a.need());
    assert!(wire.lock().unwrap().up.is_empty(), "nothing is sent while the user decides");
    a.answer(&mut s, Answer::Deny);
    assert!(a.need().is_none() && a.busy());
    assert!(last_result(&wire).contains("the user said no"));
}

#[test]
fn erasing_many_objects_asks_and_a_few_do_not() {
    let (mut s, _, _) = setup();
    let mut handles = Vec::new();
    for i in 0..25 {
        let r = s.execute("circle", &json!({"center": [i * 10, 0], "radius": 1})).unwrap();
        handles.push(r["handle"].clone());
    }
    let few = json!({"command": "erase", "params": {"handles": handles[..3]}});
    let many = json!({"command": "erase", "params": {"handles": handles}});
    assert!(tools::permission(&s, "execute", &few, false).is_none());
    assert_eq!(tools::permission(&s, "execute", &many, false).as_deref(), Some("erase 25 objects"));
    assert!(tools::permission(&s, "command_line", &json!({"text": "_.erase all"}), false).is_some());
    assert!(tools::permission(&s, "command_line", &json!({"text": "c 0,0 5"}), false).is_none());
    assert!(tools::permission(&s, "execute", &json!({"command": "lisp", "params": {"code": "(command \"erase\")"}}), true).is_some());
    assert!(tools::permission(&s, "run_lisp", &json!({"code": "(+ 1 2)"}), false).is_some());
    assert!(tools::permission(&s, "run_lisp", &json!({"code": "(+ 1 2)"}), true).is_none());
}

#[test]
fn lisp_allowed_for_the_chat_runs_without_asking_again() {
    let (mut s, mut a, wire) = setup();
    a.ask(&mut s, "add up");
    model_calls(&mut a, &mut s, &wire, "run_lisp", json!({"code": "(+ 1 2)"}));
    assert!(matches!(a.need(), Some(Need::Confirm { lisp: true, .. })));
    a.answer(&mut s, Answer::AllowLisp);
    assert!(last_result(&wire).contains("\"value\":\"3\""), "{}", last_result(&wire));
    // The next routine in the same chat runs at once.
    wire.lock().unwrap().up.push_back(ClientMsg::Ai { id: a.next_id - 1, conversation: String::new(), request: Json(json!({})) });
    model_calls(&mut a, &mut s, &wire, "run_lisp", json!({"code": "(* 2 5)"}));
    assert!(a.need().is_none());
    assert!(last_result(&wire).contains("\"value\":\"10\""), "{}", last_result(&wire));
}

#[test]
fn a_question_waits_for_the_users_answer() {
    let (mut s, mut a, wire) = setup();
    a.ask(&mut s, "draw a hole");
    model_calls(&mut a, &mut s, &wire, "ask_user", json!({"question": "What diameter?", "options": ["5", "8"]}));
    assert_eq!(a.need(), Some(&Need::Ask { question: "What diameter?".into(), options: vec!["5".into(), "8".into()] }));
    a.answer(&mut s, Answer::Text("8".into()));
    assert!(last_result(&wire).contains("\"answer\":\"8\""));
}

#[test]
fn picked_points_come_from_the_point_prompt() {
    let (mut s, mut a, wire) = setup();
    a.ask(&mut s, "put a circle where I click");
    model_calls(&mut a, &mut s, &wire, "pick_points", json!({"prompt": "Pick \"the\" centre", "count": 2}));
    assert!(matches!(a.need(), Some(Need::Points { count: 2, .. })));
    assert!(s.prompt_text().contains("Pick \"the\" centre (1 of 2)"), "{}", s.prompt_text());
    s.input(cadcraft_engine::Input::Point(cadcraft_engine::geom::Vec2::new(4.0, 5.0))).unwrap();
    assert!(!a.tick(&mut s), "still waiting for the second point");
    s.input(cadcraft_engine::Input::Point(cadcraft_engine::geom::Vec2::new(-1.5, 2.0))).unwrap();
    assert!(a.tick(&mut s));
    assert!(last_result(&wire).contains("[[4.0,5.0,0.0],[-1.5,2.0,0.0]]"), "{}", last_result(&wire));
}

#[test]
fn cancelling_a_point_pick_tells_the_model() {
    let (mut s, mut a, wire) = setup();
    a.ask(&mut s, "click somewhere");
    model_calls(&mut a, &mut s, &wire, "pick_points", json!({"prompt": "Pick"}));
    a.answer(&mut s, Answer::Cancel);
    a.tick(&mut s);
    assert!(last_result(&wire).contains("did not pick"), "{}", last_result(&wire));
    assert!(!s.lisp_running());
}

#[test]
fn picked_objects_are_the_selection() {
    let (mut s, mut a, wire) = setup();
    let h = s.execute("circle", &json!({"center": [0, 0], "radius": 1})).unwrap()["handle"].as_str().unwrap().to_string();
    a.ask(&mut s, "make this red");
    model_calls(&mut a, &mut s, &wire, "pick_objects", json!({"prompt": "Select the objects to colour"}));
    s.set_selection(vec![cadcraft_engine::doc::Handle(u64::from_str_radix(&h, 16).unwrap())]);
    a.answer(&mut s, Answer::Done);
    assert!(last_result(&wire).contains(&h), "{}", last_result(&wire));
}

#[test]
fn a_picture_goes_to_the_model_and_only_the_latest_is_kept() {
    let (mut s, mut a, wire) = setup();
    s.execute("circle", &json!({"center": [0, 0], "radius": 5})).unwrap();
    a.ask(&mut s, "look");
    model_calls(&mut a, &mut s, &wire, "render", json!({}));
    let (id, req) = last_request(&wire);
    let last = req["messages"].as_array().unwrap().last().unwrap().clone();
    assert!(last["content"][1]["image_url"]["url"].as_str().unwrap().starts_with("data:image/png;base64,"));
    // A second picture replaces the first in what is sent.
    answer(&wire, id, json!({"role": "assistant", "content": "", "tool_calls": [call("t2", "render", json!({"window": [[-1, -1], [1, 1]]}))]}));
    a.tick(&mut s);
    let (_, req) = last_request(&wire);
    let pictures = req["messages"].as_array().unwrap().iter().filter(|m| m["content"].is_array()).count();
    assert_eq!(pictures, 1);
    assert!(req.to_string().contains("no longer shown"));
}

#[test]
fn printed_point_lists_are_read() {
    assert_eq!(parse_points("((1.0 2.0 0.0) (3.5 -4.0 1.0))"), vec![[1.0, 2.0, 0.0], [3.5, -4.0, 1.0]]);
    assert_eq!(parse_points("((1.0 2.0 0.0) nil)"), vec![[1.0, 2.0, 0.0]]);
    assert!(parse_points("nil").is_empty());
    assert!(parse_points("((nan 1.0))").is_empty());
}

#[test]
fn every_skill_file_reads_and_is_in_the_prompt() {
    let all = crate::skills::all();
    assert_eq!(all.len(), 8, "a skill file has a bad header: {:?}", all.iter().map(|s| &s.name).collect::<Vec<_>>());
    let system = crate::prompt::system();
    for s in &all {
        assert!(system.contains(&format!("- {}: ", s.name)), "{}", s.name);
        assert!(s.body.starts_with("# "), "{}: {:?}", s.name, s.body.chars().take(20).collect::<String>());
    }
    assert!(crate::skills::parse("no header").is_none());
    assert!(crate::skills::parse("---\nname: x\n---\nbody").is_none(), "needs when:");
}

#[test]
fn load_skill_returns_the_body_or_the_names() {
    let (mut s, _, _) = setup();
    let out = tools::run(&mut s, "load_skill", &json!({"name": "Layer-Cleanup"}));
    assert!(out.ok && out.result.contains("laymrg"), "{}", out.result);
    let out = tools::run(&mut s, "load_skill", &json!({"name": "juggling"}));
    assert!(!out.ok && out.result.contains("dimension-a-part"));
}

#[test]
fn a_line_the_model_repeats_is_shown_once() {
    let (mut s, mut a, wire) = setup();
    a.ask(&mut s, "look twice");
    for i in 0..2 {
        let (id, _) = last_request(&wire);
        answer(&wire, id, json!({"role": "assistant", "content": "Checking.", "tool_calls": [call(&format!("c{i}"), "drawing_summary", json!({}))]}));
        a.tick(&mut s);
    }
    assert_eq!(a.log.iter().filter(|e| **e == Entry::Assistant("Checking.".into())).count(), 1);
}
