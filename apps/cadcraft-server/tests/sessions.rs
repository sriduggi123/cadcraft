//! Two engine sessions edit one drawing through a real server over WebSockets, as two desktop
//! apps do: what one draws reaches the other, both ways, and someone joining later gets it all.

use std::time::{Duration, Instant};

use cadcraft_engine::Session;
use serde_json::json;

/// A server without a data folder: its address for `cloud.share`.
fn start_server() -> String {
    format!("http://{}", start_with(cadcraft_server::Config::default()))
}

/// `s`'s drawing becomes a new cloud file on `server`: the file's WebSocket address.
fn share(s: &mut Session, server: &str, user: &str) -> String {
    let r = s.execute("cloud.share", &json!({"server": server, "user": user})).unwrap();
    assert_eq!(r["joined"], true, "{r}");
    cadcraft_engine::cmd::collab::parse_link(r["link"].as_str().unwrap()).unwrap().0
}

/// A server with `config`; its address.
fn start_with(config: cadcraft_server::Config) -> std::net::SocketAddr {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || cadcraft_server::serve(listener, cadcraft_server::Rooms::new(config)));
    addr
}

/// A plain HTTP GET: the status line and the body.
fn get(addr: std::net::SocketAddr, path: &str) -> (String, Vec<u8>) {
    use std::io::{Read, Write};
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    write!(s, "GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
    let mut all = Vec::new();
    s.read_to_end(&mut all).unwrap();
    let at = all.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let status = String::from_utf8_lossy(&all[..at]).lines().next().unwrap().to_string();
    (status, all[at + 4..].to_vec())
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("cadcraft-server-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
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
    let server = start_server();
    let mut a = Session::new();
    a.execute("circle", &json!({"center": [0, 0], "radius": 5})).unwrap();
    let url = share(&mut a, &server, "Asha");

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

/// With a data folder, a session outlives its people and a restart: who joins later gets the
/// drawing as it was left.
#[test]
fn a_saved_session_reopens_after_everyone_left_and_after_a_restart() {
    let data = temp_dir("data");
    let config = cadcraft_server::Config { web: None, data: Some(data.clone()), ai: None };
    let mut a = Session::new();
    let url = share(&mut a, &format!("http://{}", start_with(config.clone())), "Asha");
    let id = url.rsplit('/').next().unwrap().to_string();
    a.execute("circle", &json!({"center": [0, 0], "radius": 5})).unwrap();
    a.execute("line", &json!({"points": [[0, 0], [10, 0]]})).unwrap();
    wait(&mut a, "both edits confirmed", |s| s.collab_status().is_some_and(|(_, _, pending)| pending == 0));
    a.execute("collab.leave", &json!({})).unwrap();
    let file = data.join(format!("{id}.cadsession"));
    let start = Instant::now();
    let saved = |f: &std::path::Path| std::fs::read(f).ok().and_then(|b| cadcraft_collab::snapshot::decode(&b).ok()).map_or(0, |d| d.model.len());
    while saved(&file) != 2 {
        assert!(start.elapsed() < Duration::from_secs(10), "the session was not saved");
        std::thread::sleep(Duration::from_millis(20));
    }

    // A new server over the same folder, as after a restart.
    let url = format!("ws://{}/s/{id}", start_with(config));
    let mut b = Session::new();
    b.execute("collab.join", &json!({"url": url, "name": "Bo"})).unwrap();
    assert_eq!(model_len(&b), 2);
    let _ = std::fs::remove_dir_all(&data);
}

/// `/s/<session>.dwg` and `.dxf` download the live drawing, readable as a file.
#[test]
fn a_session_downloads_as_dwg_and_dxf() {
    let addr = start_with(cadcraft_server::Config::default());
    let mut a = Session::new();
    a.execute("circle", &json!({"center": [0, 0], "radius": 5})).unwrap();
    let url = share(&mut a, &format!("http://{addr}"), "Asha");
    let id = url.rsplit('/').next().unwrap().to_string();
    for ext in ["dwg", "dxf"] {
        let (status, body) = get(addr, &format!("/s/{id}.{ext}"));
        assert!(status.contains("200"), "{ext}: {status}");
        let d = cadcraft_io::read(&body, format!("out.{ext}")).unwrap();
        assert_eq!(d.model.len(), 1, "{ext}");
    }
    assert!(get(addr, "/s/none.dwg").0.contains("404"));
}

/// With the web app's folder, a session link opens the app's page and its files load.
#[test]
fn a_session_link_serves_the_web_app() {
    let web = temp_dir("web");
    std::fs::write(web.join("index.html"), "<p>app page</p>").unwrap();
    std::fs::write(web.join("app-1a2b.js"), "js").unwrap();
    let addr = start_with(cadcraft_server::Config { web: Some(web.clone()), data: None, ai: None });
    let (status, body) = get(addr, "/s/plan?name=Bo");
    assert!(status.contains("200"), "{status}");
    assert_eq!(body, b"<p>app page</p>");
    assert_eq!(get(addr, "/s/app-1a2b.js").1, b"js");
    assert!(get(addr, "/s/missing.js").0.contains("404"));
    let _ = std::fs::remove_dir_all(&web);
}

/// Cloud files by link: Collaborate makes a file with a long random id and gives its link;
/// another app opens the link and edits; the status answers only about ids asked for; the
/// front page lists nothing; a made-up id is refused.
#[test]
fn files_are_shared_and_opened_by_link_only() {
    let data = temp_dir("cloud");
    let addr = start_with(cadcraft_server::Config { web: None, data: Some(data.clone()), ai: None });
    let server = format!("http://{addr}");

    let mut a = Session::new();
    a.execute("circle", &json!({"center": [0, 0], "radius": 5})).unwrap();
    let shared = a.execute("cloud.share", &json!({"title": "Floor plan", "server": server, "user": "Asha"})).unwrap();
    let link = shared["link"].as_str().unwrap().to_string();
    assert!(link.starts_with(&format!("http://{addr}/s/")), "{link}");
    let id = link.rsplit('/').next().unwrap().to_string();
    assert_eq!(id.len(), 22, "a 128-bit id");
    assert_eq!(shared["title"], "Floor plan");
    assert!(data.join(format!("{id}.cadsession")).exists(), "on disk at once");

    let mut b = Session::new();
    let tabs = b.docs.len();
    b.execute("cloud.open", &json!({"link": link, "user": "Bo"})).unwrap();
    assert_eq!(b.docs.len(), tabs + 1, "opens in a new tab");
    assert_eq!(b.state().unwrap().title, "Floor plan");
    assert_eq!(model_len(&b), 1);
    b.execute("line", &json!({"points": [[0, 0], [10, 0]]})).unwrap();
    wait(&mut a, "Bo's line at Asha", |s| model_len(s) == 2);

    let status = a.execute("cloud.status", &json!({"links": [link, format!("{server}/s/made-up-id")]})).unwrap();
    assert_eq!(status["files"].as_array().map(Vec::len), Some(1), "{status}");
    assert_eq!(status["files"][0]["title"], "Floor plan");
    assert_eq!(status["files"][0]["people"], json!(["Asha", "Bo"]));

    let mut c = Session::new();
    let err = c.execute("cloud.open", &json!({"link": format!("{server}/s/made-up-id")})).unwrap_err();
    assert!(err.to_string().contains("no file with this link"), "{err}");
    let (status, page) = get(addr, "/");
    assert!(status.contains("200"));
    assert!(!String::from_utf8_lossy(&page).contains(&id), "the front page lists nothing");
    let _ = std::fs::remove_dir_all(&data);
}

#[test]
fn cloud_server_addresses_are_normalised() {
    use cadcraft_engine::cmd::collab::cloud_server;
    assert_eq!(cloud_server(&json!({"server": "https://cad.example.com/"})), "wss://cad.example.com");
    assert_eq!(cloud_server(&json!({"server": "http://10.0.0.2:9010"})), "ws://10.0.0.2:9010");
    assert_eq!(cloud_server(&json!({"server": "cad.example.com:9010"})), "ws://cad.example.com:9010");
    assert_eq!(cloud_server(&json!({"server": "wss://x"})), "wss://x");
    assert_eq!(cloud_server(&json!({"server": "cad.example.com"})), "wss://cad.example.com");
    assert_eq!(cloud_server(&json!({"server": "localhost:9010"})), "ws://localhost:9010");
    use cadcraft_engine::cmd::collab::{parse_link, share_link};
    let want = ("wss://cad.example.com/s/Ab-9_x".to_string(), "Ab-9_x".to_string());
    for link in ["https://cad.example.com/s/Ab-9_x", "cad.example.com/s/Ab-9_x/", "wss://cad.example.com/s/Ab-9_x?name=Bo#x"] {
        assert_eq!(parse_link(link).unwrap(), want, "{link}");
    }
    assert!(parse_link("https://cad.example.com/files").is_err());
    assert!(parse_link("https://cad.example.com/s/../x").is_err());
    assert_eq!(share_link("wss://cad.example.com/s/Ab"), "https://cad.example.com/s/Ab");
}

/// Presence: what one person points at, has selected and runs reaches the others; someone who
/// joins later sees where everyone already is; following copies the view until the follower
/// zooms; an unreachable server fails within the connection timeout.
#[test]
fn presence_reaches_the_others_and_follow_copies_the_view() {
    let server = start_server();
    let mut a = Session::new();
    a.execute("circle", &json!({"center": [0, 0], "radius": 5})).unwrap();
    let url = share(&mut a, &server, "Asha");
    let circle = a.state().unwrap().doc.model.last().map(|e| e.handle).unwrap();
    a.state_mut().unwrap().selection = vec![circle];
    a.collab_presence(Some(cadcraft_engine::geom::Vec2::new(3.0, 4.0)), 1.0);

    let mut b = Session::new();
    b.execute("collab.join", &json!({"url": url, "name": "Bo"})).unwrap();
    let seen = |s: &Session| s.collab_others().into_iter().find(|(p, _)| p.name == "Asha").map(|(_, pr)| pr);
    wait(&mut b, "Asha's presence at the late joiner", |s| seen(s).is_some_and(|p| p.cursor == Some([3.0, 4.0])));
    assert_eq!(seen(&b).unwrap().selection, vec![circle]);
    let peers = b.execute("collab.peers", &json!({})).unwrap();
    assert_eq!(peers["peers"][0]["cursor"], json!([3.0, 4.0]), "{peers}");

    // Asha starts a line: Bo sees its rubber band from her first point to her cursor.
    a.cmdline("line").unwrap();
    a.cmdline("0,0").unwrap();
    a.collab_presence(Some(cadcraft_engine::geom::Vec2::new(5.0, 5.0)), 1.5);
    wait(&mut b, "Asha's rubber band", |s| seen(s).is_some_and(|p| p.command.as_deref() == Some("LINE") && !p.preview.is_empty()));
    a.cancel();

    // Bo follows Asha; her zoom moves his view.
    b.execute("collab.follow", &json!({"name": "asha"})).unwrap();
    a.execute("zoom", &json!({"mode": "center", "center": [1, 1], "height": 2})).unwrap();
    a.collab_presence(None, 2.0);
    let ha = a.state().unwrap().view().height;
    wait(&mut b, "Bo's view to follow Asha's", |s| s.state().is_ok_and(|st| (st.view().height - ha).abs() < 1e-9));
    assert!(b.collab_following().is_some());

    // Bo zooms himself: following stops.
    b.execute("zoom", &json!({"mode": "extents"})).unwrap();
    b.collab_tick();
    assert_eq!(b.collab_following(), None);
    assert!(b.execute("collab.follow", &json!({"name": "Nobody"})).is_err());

    // A 10.255.255.1-style unroutable address fails within the timeout instead of hanging.
    let start = Instant::now();
    assert!(cadcraft_engine::cmd::collab::cloud_connect("ws://10.255.255.1:9/s/x").is_err());
    assert!(start.elapsed() < Duration::from_secs(20), "connecting took {:?}", start.elapsed());
}

/// A relay in front of the server whose connections can be cut, as a dropped Wi-Fi does.
struct Relay {
    addr: std::net::SocketAddr,
    conns: std::sync::Arc<std::sync::Mutex<Vec<std::net::TcpStream>>>,
}

impl Relay {
    fn new(to: std::net::SocketAddr) -> Relay {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let conns = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let keep = conns.clone();
        std::thread::spawn(move || {
            for down in listener.incoming().flatten() {
                let up = std::net::TcpStream::connect(to).unwrap();
                keep.lock().unwrap().extend([down.try_clone().unwrap(), up.try_clone().unwrap()]);
                for (mut from, mut to) in [(down.try_clone().unwrap(), up.try_clone().unwrap()), (up, down)] {
                    std::thread::spawn(move || {
                        let _ = std::io::copy(&mut from, &mut to);
                        let _ = to.shutdown(std::net::Shutdown::Both);
                    });
                }
            }
        });
        Relay { addr, conns }
    }

    fn cut(&self) {
        for s in self.conns.lock().unwrap().drain(..) {
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
    }
}

/// A dropped connection comes back by itself: the person keeps their client id, what they drew
/// while away reaches the others, and what the others drew meanwhile reaches them.
#[test]
fn a_dropped_connection_reconnects_and_catches_up() {
    let server = start_with(cadcraft_server::Config::default());
    let relay = Relay::new(server);
    let mut a = Session::new();
    let via = share(&mut a, &format!("http://{}", relay.addr), "Asha");
    let id = via.rsplit('/').next().unwrap().to_string();
    let mut b = Session::new();
    b.execute("collab.join", &json!({"url": format!("ws://{server}/s/{id}"), "name": "Bo"})).unwrap();
    let id = a.collab_status().map(|(c, _, _)| c);
    a.execute("circle", &json!({"center": [0, 0], "radius": 5})).unwrap();
    wait(&mut b, "Asha's circle", |s| model_len(s) == 1);

    relay.cut();
    wait(&mut a, "Asha to notice the drop", Session::collab_reconnecting);
    a.execute("line", &json!({"points": [[0, 0], [10, 0]]})).unwrap();
    b.execute("text", &json!({"at": [3, 3], "text": "from Bo", "height": 2})).unwrap();

    wait(&mut a, "Asha back with Bo's text", |s| !s.collab_reconnecting() && model_len(s) == 3);
    assert_eq!(a.collab_status().map(|(c, _, _)| c), id, "Asha keeps her client id");
    assert!(a.log.iter().any(|l| l == "Reconnected."), "{:?}", a.log);
    wait(&mut b, "the line Asha drew while away", |s| model_len(s) == 3);
    a.execute("circle", &json!({"center": [20, 0], "radius": 2})).unwrap();
    wait(&mut b, "Asha's next edit", |s| model_len(s) == 4);
}

/// A pretend AI provider: answers each chat request with the next of `replies` and keeps what it
/// was sent (headers and body).
fn fake_provider(replies: Vec<serde_json::Value>) -> (String, std::sync::Arc<std::sync::Mutex<Vec<(String, serde_json::Value)>>>) {
    use std::io::{BufRead, BufReader, Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1/chat/completions", listener.local_addr().unwrap());
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for (stream, reply) in listener.incoming().zip(replies) {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut head = String::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" || line.is_empty() {
                    break;
                }
                head.push_str(&line);
            }
            let lower = head.to_ascii_lowercase();
            if lower.contains("expect: 100-continue") {
                stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n").unwrap();
            }
            let len: usize = lower.lines().find_map(|l| l.strip_prefix("content-length:")).map(|v| v.trim().parse().unwrap()).unwrap_or(0);
            let mut body = vec![0; len];
            reader.read_exact(&mut body).unwrap();
            log.lock().unwrap().push((head, serde_json::from_slice(&body).unwrap()));
            let out = reply.to_string();
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{out}", out.len())
                .unwrap();
        }
    });
    (url, seen)
}

fn completion(message: serde_json::Value) -> serde_json::Value {
    json!({"choices": [{"index": 0, "message": message, "finish_reason": "stop"}], "usage": {"total_tokens": 10}})
}

#[test]
fn the_assistant_edits_the_drawing_through_the_server_which_holds_the_key() {
    let circle = json!({"id": "call_1", "type": "function", "function": {"name": "execute",
        "arguments": json!({"command": "circle", "params": {"center": [3, 4], "radius": 2}}).to_string()}});
    let (endpoint, seen) = fake_provider(vec![
        completion(json!({"role": "assistant", "content": "", "tool_calls": [circle]})),
        completion(json!({"role": "assistant", "content": "Drew a circle of radius 2 at 3,4."})),
    ]);
    let ai = cadcraft_server::ai::AiConfig { endpoint, model: "test-model".into(), key: "secret-key".into() };
    let addr = start_with(cadcraft_server::Config { web: None, data: None, ai: Some(ai) });
    let mut s = Session::new();
    s.execute("new", &json!({})).unwrap();
    let mut a = cadcraft_ai::Assistant::new(&format!("ws://{addr}"));
    a.ask(&mut s, "draw a circle");
    let until = Instant::now() + Duration::from_secs(30);
    while a.busy() && Instant::now() < until {
        a.tick(&mut s);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!a.busy(), "no answer: {:?}", a.log);
    // The answer, then how long the request took.
    let answer = a.log.iter().rev().find(|e| matches!(e, cadcraft_ai::Entry::Assistant(_)));
    assert_eq!(answer, Some(&cadcraft_ai::Entry::Assistant("Drew a circle of radius 2 at 3,4.".into())), "{:?}", a.log);
    assert!(matches!(a.log.last(), Some(cadcraft_ai::Entry::Tool { line, ok: true }) if line.starts_with("Done in")), "{:?}", a.log);
    assert_eq!(model_len(&s), 1);

    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    let (head, body) = &seen[0];
    // The server adds the key and the model; the app sends neither.
    assert!(head.contains("Authorization: Bearer secret-key"), "{head}");
    assert!(head.contains("User-Agent: cadcraft-ai/"), "{head}");
    assert!(head.contains("x-opencode-session: cadcraft-"), "{head}");
    assert_eq!(body["model"], "test-model");
    assert!(body["tools"].as_array().is_some_and(|t| !t.is_empty()));
    // The second request carries the tool's result.
    let msgs = seen[1].1["messages"].as_array().unwrap();
    assert_eq!(msgs.last().unwrap()["tool_call_id"], "call_1");
}

#[test]
fn without_a_key_the_server_says_the_assistant_is_off() {
    let addr = start_with(cadcraft_server::Config::default());
    let mut s = Session::new();
    s.execute("new", &json!({})).unwrap();
    let mut a = cadcraft_ai::Assistant::new(&format!("ws://{addr}"));
    a.ask(&mut s, "hello");
    let until = Instant::now() + Duration::from_secs(10);
    while a.busy() && Instant::now() < until {
        a.tick(&mut s);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(a.log.last(), Some(&cadcraft_ai::Entry::Error("this server has no AI set up".into())));
}
