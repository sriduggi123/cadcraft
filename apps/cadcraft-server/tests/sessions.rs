//! Two engine sessions edit one drawing through a real server over WebSockets, as two desktop
//! apps do: what one draws reaches the other, both ways, and someone joining later gets it all.

use std::time::{Duration, Instant};

use cadcraft_engine::Session;
use serde_json::json;

fn start_server() -> String {
    format!("ws://{}/s/test-plan", start_with(cadcraft_server::Config::default()))
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

/// With a data folder, a session outlives its people and a restart: who joins later gets the
/// drawing as it was left.
#[test]
fn a_saved_session_reopens_after_everyone_left_and_after_a_restart() {
    let data = temp_dir("data");
    let config = cadcraft_server::Config { web: None, data: Some(data.clone()) };
    let url = format!("ws://{}/s/kept", start_with(config.clone()));
    let mut a = Session::new();
    a.execute("collab.join", &json!({"url": url, "name": "Asha", "host": true})).unwrap();
    a.execute("circle", &json!({"center": [0, 0], "radius": 5})).unwrap();
    a.execute("line", &json!({"points": [[0, 0], [10, 0]]})).unwrap();
    wait(&mut a, "both edits confirmed", |s| s.collab_status().is_some_and(|(_, _, pending)| pending == 0));
    a.execute("collab.leave", &json!({})).unwrap();
    let file = data.join("kept.cadsession");
    let start = Instant::now();
    while !file.exists() {
        assert!(start.elapsed() < Duration::from_secs(10), "the session was not saved");
        std::thread::sleep(Duration::from_millis(20));
    }

    // A new server over the same folder, as after a restart; the joiner's own drawing is ignored.
    let url = format!("ws://{}/s/kept", start_with(config));
    let mut b = Session::new();
    b.execute("collab.join", &json!({"url": url, "name": "Bo", "host": true})).unwrap();
    assert_eq!(model_len(&b), 2);
    let _ = std::fs::remove_dir_all(&data);
}

/// `/s/<session>.dwg` and `.dxf` download the live drawing, readable as a file.
#[test]
fn a_session_downloads_as_dwg_and_dxf() {
    let addr = start_with(cadcraft_server::Config::default());
    let mut a = Session::new();
    a.execute("circle", &json!({"center": [0, 0], "radius": 5})).unwrap();
    a.execute("collab.join", &json!({"url": format!("ws://{addr}/s/out"), "name": "Asha", "host": true})).unwrap();
    for ext in ["dwg", "dxf"] {
        let (status, body) = get(addr, &format!("/s/out.{ext}"));
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
    let addr = start_with(cadcraft_server::Config { web: Some(web.clone()), data: None });
    let (status, body) = get(addr, "/s/plan?name=Bo");
    assert!(status.contains("200"), "{status}");
    assert_eq!(body, b"<p>app page</p>");
    assert_eq!(get(addr, "/s/app-1a2b.js").1, b"js");
    assert!(get(addr, "/s/missing.js").0.contains("404"));
    let _ = std::fs::remove_dir_all(&web);
}

/// Cloud files: save a drawing to the cloud, see it in the list with who has it open, open it
/// from another app, and a second save under the same name is refused.
#[test]
fn cloud_files_are_saved_listed_and_opened() {
    let data = temp_dir("cloud");
    let addr = start_with(cadcraft_server::Config { web: None, data: Some(data.clone()) });
    let server = format!("http://{addr}");

    let mut a = Session::new();
    a.execute("circle", &json!({"center": [0, 0], "radius": 5})).unwrap();
    let saved = a.execute("cloud.save", &json!({"name": "plan", "server": server, "user": "Asha"})).unwrap();
    assert_eq!(saved["joined"], true, "{saved}");
    assert_eq!(a.state().unwrap().title, "plan");

    let list = a.execute("cloud.list", &json!({"server": server})).unwrap();
    assert_eq!(list["files"][0]["name"], "plan", "{list}");
    assert_eq!(list["files"][0]["people"], json!(["Asha"]), "{list}");

    let mut b = Session::new();
    let tabs = b.docs.len();
    b.execute("cloud.open", &json!({"name": "plan", "server": server, "user": "Bo"})).unwrap();
    assert_eq!(b.docs.len(), tabs + 1, "opens in a new tab");
    assert_eq!(b.state().unwrap().title, "plan");
    assert_eq!(model_len(&b), 1);
    b.execute("line", &json!({"points": [[0, 0], [10, 0]]})).unwrap();
    wait(&mut a, "Bo's line at Asha", |s| model_len(s) == 2);

    let mut c = Session::new();
    let err = c.execute("cloud.save", &json!({"name": "plan", "server": server, "user": "Cy"})).unwrap_err();
    assert!(err.to_string().contains("already exists"), "{err}");

    let (status, page) = get(addr, "/");
    assert!(status.contains("200"));
    let page = String::from_utf8_lossy(&page);
    assert!(page.contains("plan") && page.contains("2 editing"), "{page}");
    let _ = std::fs::remove_dir_all(&data);
}

#[test]
fn cloud_server_addresses_are_normalised() {
    use cadcraft_engine::cmd::collab::cloud_server;
    assert_eq!(cloud_server(&json!({"server": "https://cad.example.com/"})), "wss://cad.example.com");
    assert_eq!(cloud_server(&json!({"server": "http://10.0.0.2:9010"})), "ws://10.0.0.2:9010");
    assert_eq!(cloud_server(&json!({"server": "cad.example.com:9010"})), "ws://cad.example.com:9010");
    assert_eq!(cloud_server(&json!({"server": "wss://x"})), "wss://x");
}
