//! Two engine sessions edit one drawing through a real server over WebSockets, as two desktop
//! apps do: what one draws reaches the other, both ways, and someone joining later gets it all.

use std::time::{Duration, Instant};

use cadcraft_engine::Session;
use serde_json::json;

fn start_server() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || cadcraft_server::serve(listener, cadcraft_server::Rooms::default()));
    format!("ws://{addr}/s/test-plan")
}

/// Tick `s` until `ok` holds (or fail after a few seconds).
fn wait(s: &mut Session, what: &str, ok: impl Fn(&Session) -> bool) {
    let start = Instant::now();
    while !ok(s) {
        assert!(start.elapsed() < Duration::from_secs(10), "timed out waiting for {what}");
        s.collab_tick();
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn model_len(s: &Session) -> usize {
    s.state().map(|st| st.doc.model.len()).unwrap_or(0)
}

#[test]
fn two_sessions_edit_one_drawing_over_the_network() {
    let url = start_server();
    let mut a = Session::new();
    a.execute("circle", &json!({"center": [0, 0], "radius": 5})).unwrap();
    let joined = a.execute("collab.join", &json!({"url": url, "name": "Asha", "host": true})).unwrap();
    assert_eq!(joined["joined"], true, "{joined}");

    let mut b = Session::new();
    b.execute("collab.join", &json!({"url": url, "name": "Bo"})).unwrap();
    assert_eq!(model_len(&b), 1, "the late joiner gets the host's drawing");

    a.execute("line", &json!({"points": [[0, 0], [10, 0]]})).unwrap();
    wait(&mut b, "A's line at B", |s| model_len(s) == 2);

    let h = b.state().unwrap().doc.model.last().map(|e| e.handle).unwrap();
    b.execute("move", &json!({"handles": [h.hex()], "delta": [0, 5]})).unwrap();
    b.execute("text", &json!({"at": [3, 3], "text": "from Bo", "height": 2})).unwrap();
    wait(&mut a, "B's edits at A", |s| model_len(s) == 3);
    let (da, db) = (a.state().unwrap().doc.clone(), b.state().unwrap().doc.clone());
    assert_eq!(da.model.get(h), db.model.get(h), "the moved line is the same on both");

    let peers = a.execute("collab.peers", &json!({})).unwrap();
    assert_eq!(peers["peers"].as_array().map(Vec::len), Some(2), "{peers}");

    let mut c = Session::new();
    c.execute("collab.join", &json!({"url": url, "name": "Cy"})).unwrap();
    assert_eq!(model_len(&c), 3);
}

#[test]
fn session_names_are_checked() {
    assert_eq!(cadcraft_server::session_name("/s/office-plan_2").as_deref(), Some("office-plan_2"));
    for bad in ["/", "/s/", "/s/a/b", "/x/plan", "/s/plan?x", "/s/../etc"] {
        assert_eq!(cadcraft_server::session_name(bad), None, "{bad}");
    }
}
