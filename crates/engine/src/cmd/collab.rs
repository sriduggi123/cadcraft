//! Shared-session commands (the fork's multiplayer work): join a session server, leave it, and
//! list who is in it; and cloud files. A cloud file lives on the server under a long random id;
//! its link (`https://host/s/<id>`) is the key to it, and nobody can list the server's files.
//! COLLABORATE puts the active drawing on the server and gives its link; OPEN LINK joins one in a
//! new tab. Ordinary commands, so the CLI, the control channel and MCP can use them too.

#[cfg(not(target_arch = "wasm32"))]
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::{bad, str_param};
use crate::{CommandSpec, EngineError, Result, Session};

/// How long `collab.join` waits for the server's welcome.
#[cfg(not(target_arch = "wasm32"))]
const JOIN_WAIT: Duration = Duration::from_secs(20);
/// How long `cloud.status` waits for an answer.
#[cfg(not(target_arch = "wasm32"))]
const STATUS_WAIT: Duration = Duration::from_secs(10);

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("collab.join", "Join Shared Session", run_join)
            .params("{url: ws:// or wss:// address of a file (…/s/<id>), name?: shown to others} → {client, seq, peers}")
            .noundo(),
        CommandSpec::new("collab.leave", "Leave Shared Session", run_leave).params("{} → {left}").noundo(),
        CommandSpec::new("collab.peers", "Shared Session Status", run_peers)
            .params("{} → {live, joined, client?, link?, title?, peers: [{client, name, layout?, cursor?, selection?, command?, view?}], following?, pending, ended?}")
            .noundo(),
        CommandSpec::new("collab.follow", "Follow in Shared Session", run_follow)
            .params("{name?: person (or client id) to follow; omit to stop} → {following?} (panning or zooming stops it)")
            .noundo(),
        CommandSpec::new("cloud.share", "Collaborate", run_cloud_share)
            .params("{title?: default the tab's name, server?: address (default $CADCRAFT_CLOUD or the built-in server), user?: shown to others} → {link, title, client, peers} (the active drawing becomes a new cloud file; anyone with the link can open and edit it)")
            .noundo(),
        CommandSpec::new("cloud.open", "Open Link", run_cloud_open)
            .params("{link: https://host/s/<id> (or ws://, wss://, host/s/<id>), user?} → {link, title, client, peers} (opens in a new tab)")
            .noundo(),
        CommandSpec::new("cloud.status", "Cloud File Status", run_cloud_status)
            .params("{links: [link]} → {files: [{link, title, modified: seconds since 1970, people: [name]}]} (only the files that exist)")
            .noundo(),
    ]
}

/// The server used when none is given: `$CADCRAFT_CLOUD`, else this.
pub const DEFAULT_SERVER: &str = "ws://127.0.0.1:9010";

/// A server address as `ws://` or `wss://` without a trailing `/`. `https`/`http` become
/// `wss`/`ws`; a bare host is `wss://`, except a local one or one with a port (`ws://`).
pub fn normalize_server(raw: &str) -> String {
    let raw = raw.trim().trim_end_matches('/');
    if let Some(rest) = raw.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = raw.strip_prefix("http://") {
        format!("ws://{rest}")
    } else if raw.starts_with("ws://") || raw.starts_with("wss://") {
        raw.to_string()
    } else {
        let local = raw.starts_with("localhost") || raw.starts_with("127.") || raw.starts_with('[') || raw.contains(':');
        format!("{}://{raw}", if local { "ws" } else { "wss" })
    }
}

/// The cloud server's address from `server`, `$CADCRAFT_CLOUD` or [`DEFAULT_SERVER`].
pub fn cloud_server(p: &Value) -> String {
    let raw = str_param(p, "server").map(str::to_string).or_else(|| std::env::var("CADCRAFT_CLOUD").ok()).unwrap_or_else(|| DEFAULT_SERVER.into());
    normalize_server(&raw)
}

/// Whether `id` can be a file id: 1 to 64 letters, digits, `-` and `_`.
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// A link to a cloud file as the WebSocket address the apps connect to, and the file's id.
/// Accepts `https://host/s/<id>`, `http://`, `ws://`, `wss://`, `cadcraft://` and
/// `host/s/<id>`, with any `?…` or `#…` after it.
pub fn parse_link(link: &str) -> Result<(String, String)> {
    let link = link.trim();
    let link = link.split(['?', '#']).next().unwrap_or_default().trim_end_matches('/');
    // `cadcraft://host/s/<id>`, for a link handed to the app by the system, is `https://`.
    let https;
    let link = match link.strip_prefix("cadcraft://") {
        Some(rest) => {
            https = format!("https://{rest}");
            https.as_str()
        }
        None => link,
    };
    let (server, id) = link.rsplit_once("/s/").ok_or_else(|| bad("cloud.open", "that is not a CADCraft file link (…/s/<id>)"))?;
    if server.is_empty() || !valid_id(id) {
        return Err(bad("cloud.open", "that is not a CADCraft file link (…/s/<id>)"));
    }
    let server = normalize_server(server);
    Ok((format!("{server}/s/{id}"), id.to_string()))
}

/// The link people share for a file's WebSocket address: `wss://` → `https://`, `ws://` →
/// `http://` (it opens the file in a browser too).
pub fn share_link(url: &str) -> String {
    if let Some(rest) = url.strip_prefix("wss://") {
        format!("https://{rest}")
    } else if let Some(rest) = url.strip_prefix("ws://") {
        format!("http://{rest}")
    } else {
        url.to_string()
    }
}

pub fn cloud_user(p: &Value) -> String {
    str_param(p, "user")
        .map(str::to_string)
        .or_else(|| std::env::var("USER").ok())
        .or_else(|| std::env::var("USERNAME").ok())
        .unwrap_or_else(|| "CADCraft user".into())
}

/// A title for the active drawing: its tab's name without the extension.
pub fn default_title(s: &Session) -> String {
    let title = s.state().map(|st| st.title.clone()).unwrap_or_default();
    title.rsplit_once('.').map_or(title.as_str(), |(stem, _)| stem).trim().to_string()
}

/// Ask `server` about files `ids`; [`status_poll`] reads the answer (the browser does on later
/// frames).
pub fn status_start(server: &str, ids: &[String]) -> Result<Box<dyn cadcraft_collab::Link>> {
    let mut link = cloud_connect(&format!("{server}/status"))?;
    link.send(&cadcraft_collab::message::ClientMsg::List { files: ids.to_vec() }).map_err(|e| EngineError::Other(e.to_string()))?;
    Ok(link)
}

/// The answer to [`status_start`]: `[{id, title, modified, people}]`, an error, or `None` while
/// it hasn't come.
pub fn status_poll(link: &mut dyn cadcraft_collab::Link) -> Option<Result<Vec<Value>>> {
    use cadcraft_collab::message::ServerMsg;
    let fail = |e: String| Some(Err(EngineError::Other(e)));
    for _ in 0..16 {
        match link.recv() {
            Some(Ok(ServerMsg::Files { files })) => {
                return Some(Ok(files.iter().map(|f| json!({"id": f.name, "title": f.title, "modified": f.modified, "people": f.people})).collect()));
            }
            Some(Ok(ServerMsg::Error { message })) => return fail(message),
            Some(Ok(_)) => {}
            Some(Err(e)) => return fail(e.to_string()),
            None if !link.is_open() => return fail("the server closed the connection".into()),
            None => return None,
        }
    }
    None
}

/// How the files behind `links` are doing, by link (it waits on the network: the desktop UI
/// calls it from a background thread). Files on several servers are asked about server by server.
#[cfg(not(target_arch = "wasm32"))]
pub fn file_status(links: &[String]) -> Result<Vec<Value>> {
    let mut by_server: std::collections::BTreeMap<String, Vec<String>> = std::collections::BTreeMap::new();
    for l in links {
        if let Ok((url, id)) = parse_link(l)
            && let Some(server) = url.strip_suffix(&format!("/s/{id}"))
        {
            by_server.entry(server.to_string()).or_default().push(id);
        }
    }
    let mut out = Vec::new();
    for (server, ids) in by_server {
        let Ok(mut link) = status_start(&server, &ids) else { continue };
        let start = Instant::now();
        let files = loop {
            if let Some(r) = status_poll(link.as_mut()) {
                break r.unwrap_or_default();
            }
            if start.elapsed() > STATUS_WAIT {
                break Vec::new();
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        for mut f in files {
            let id = f.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
            f["link"] = json!(share_link(&format!("{server}/s/{id}")));
            out.push(f);
        }
    }
    Ok(out)
}

#[cfg(not(target_arch = "wasm32"))]
fn run_cloud_status(_: &mut Session, p: &Value) -> Result<Value> {
    let links: Vec<String> =
        p.get("links").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default();
    Ok(json!({ "files": file_status(&links)? }))
}

#[cfg(target_arch = "wasm32")]
fn run_cloud_status(_: &mut Session, _: &Value) -> Result<Value> {
    Err(bad("cloud.status", "not available in the browser"))
}

/// Open a cloud file in a new tab, or make the active drawing a new one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloudJoin {
    Open,
    Share,
}

/// Connect to `url`; it waits on the network (the desktop UI calls it from a background thread).
pub fn cloud_connect(url: &str) -> Result<Box<dyn cadcraft_collab::Link>> {
    cadcraft_collab_net::connect(url).map_err(|e| EngineError::Other(e.to_string()))
}

/// Start joining over `link` without waiting: open the file at `url` in a new tab, or share the
/// active drawing (`url` is the server's `…/new`) as `title`. The welcome arrives through
/// [`Session::collab_tick`], then call [`cloud_joined`].
pub fn cloud_begin(s: &mut Session, how: CloudJoin, link: Box<dyn cadcraft_collab::Link>, url: &str, user: &str, title: &str) -> Result<()> {
    match how {
        CloudJoin::Open => {
            // One shared drawing at a time: the one open before stays in its tab as a local copy.
            if s.collab_live() {
                s.collab_leave();
                s.echo("Left the previous cloud file; its tab is now a local copy.".to_string());
            }
            s.open_drawing(cadcraft_doc::Drawing::new_metric(), "Cloud file", None);
            s.collab_attach_to(link, url, user, None)
        }
        CloudJoin::Share => {
            if s.collab_live() {
                return Err(bad("cloud.share", "a drawing is already shared; leave it first (collab.leave)"));
            }
            s.collab_attach_to(link, url, user, Some(title))
        }
    }
}

/// After the welcome: fit an opened drawing in the window.
pub fn cloud_joined(s: &mut Session, how: CloudJoin) {
    if how == CloudJoin::Open {
        let _ = s.execute("zoom", &json!({"mode": "extents"}));
    }
}

/// The shared file's link, once joined.
pub fn current_link(s: &Session) -> Option<String> {
    s.collab_url().filter(|u| u.contains("/s/")).map(share_link)
}

fn run_cloud_share(s: &mut Session, p: &Value) -> Result<Value> {
    if s.collab_live() {
        return Err(bad("cloud.share", "a drawing is already shared; leave it first (collab.leave)"));
    }
    let title = str_param(p, "title").map(str::trim).filter(|t| !t.is_empty()).map_or_else(|| default_title(s), str::to_string);
    let url = format!("{}/new", cloud_server(p));
    cloud_run(s, CloudJoin::Share, &url, &cloud_user(p), &title)
}

fn run_cloud_open(s: &mut Session, p: &Value) -> Result<Value> {
    let link = str_param(p, "link").ok_or_else(|| bad("cloud.open", "link is required"))?;
    let (url, _) = parse_link(link)?;
    if s.collab_url() == Some(url.as_str()) {
        if let Some(i) = s.collab_uid().and_then(|uid| s.docs.iter().position(|d| d.uid == uid)) {
            s.active = i;
        }
        return cloud_result(s);
    }
    cloud_run(s, CloudJoin::Open, &url, &cloud_user(p), "")
}

fn cloud_run(s: &mut Session, how: CloudJoin, url: &str, user: &str, title: &str) -> Result<Value> {
    let link = cloud_connect(url)?;
    cloud_begin(s, how, link, url, user, title)?;
    wait_for_welcome(s)?;
    #[cfg(not(target_arch = "wasm32"))]
    cloud_joined(s, how);
    cloud_result(s)
}

fn cloud_result(s: &mut Session) -> Result<Value> {
    let mut r = run_peers(s, &json!({}))?;
    r["link"] = json!(current_link(s));
    r["title"] = json!(s.collab_title());
    Ok(r)
}

fn run_join(s: &mut Session, p: &Value) -> Result<Value> {
    let url = str_param(p, "url").ok_or_else(|| bad("collab.join", "url is required"))?;
    if !(url.starts_with("ws://") || url.starts_with("wss://")) {
        return Err(bad("collab.join", "the url must start with ws:// or wss://"));
    }
    let name = str_param(p, "name").unwrap_or("CADCraft user");
    let link = cadcraft_collab_net::connect(url).map_err(|e| EngineError::Other(e.to_string()))?;
    s.collab_attach_to(link, url, name, None)?;
    wait_for_welcome(s)?;
    run_peers(s, &json!({}))
}

/// Natively, wait for the welcome. The browser can't block: the welcome arrives through a later
/// frame's `collab_tick`, and `joined` is false until then.
#[cfg(target_arch = "wasm32")]
fn wait_for_welcome(_: &mut Session) -> Result<()> {
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn wait_for_welcome(s: &mut Session) -> Result<()> {
    let start = Instant::now();
    while !s.collab_joined() {
        s.collab_tick();
        if let Some(why) = s.collab_ended() {
            return Err(EngineError::Other(format!("could not join: {why}")));
        }
        if start.elapsed() > JOIN_WAIT {
            s.collab_leave();
            return Err(EngineError::Other("the server did not answer in time".into()));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

fn run_leave(s: &mut Session, _: &Value) -> Result<Value> {
    let left = s.collab_live();
    s.collab_leave();
    Ok(json!({ "left": left }))
}

fn run_follow(s: &mut Session, p: &Value) -> Result<Value> {
    let who = p.get("name").and_then(|v| v.as_str().map(str::to_string).or_else(|| v.as_u64().map(|n| n.to_string())));
    let Some(who) = who.map(|w| w.trim().to_string()).filter(|w| !w.is_empty()) else {
        s.collab_follow(None);
        return Ok(json!({ "following": null }));
    };
    if !s.collab_joined() {
        return Err(bad("collab.follow", "not in a shared session"));
    }
    let others = s.collab_others();
    let found = others.iter().find(|(p, _)| p.client.to_string() == who).or_else(|| others.iter().find(|(p, _)| p.name.eq_ignore_ascii_case(&who)));
    let Some((peer, _)) = found else { return Err(bad("collab.follow", format!("nobody named {who} is in the session"))) };
    let (client, name) = (peer.client, peer.name.clone());
    s.collab_follow(Some(client));
    s.echo(format!("Following {name}; pan or zoom to stop."));
    Ok(json!({ "following": {"client": client, "name": name} }))
}

fn run_peers(s: &mut Session, _: &Value) -> Result<Value> {
    let status = s.collab_status();
    let others = s.collab_others();
    let peers: Vec<Value> = status
        .as_ref()
        .map(|(_, p, _)| {
            p.iter()
                .map(|p| {
                    let mut v = json!({"client": p.client, "name": p.name});
                    if let Some((_, pr)) = others.iter().find(|(o, _)| o.client == p.client) {
                        v["layout"] = json!(pr.layout);
                        v["cursor"] = json!(pr.cursor);
                        v["selection"] = json!(pr.selection.iter().map(|h| format!("{:X}", h.0)).collect::<Vec<_>>());
                        v["command"] = json!(pr.command);
                        v["view"] = json!(pr.view);
                    }
                    v
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(json!({
        "live": s.collab_live(),
        "joined": s.collab_joined(),
        "client": status.as_ref().map(|(c, _, _)| *c),
        "peers": peers,
        "following": s.collab_following(),
        "link": current_link(s),
        "title": s.collab_title(),
        "pending": status.as_ref().map_or(0, |(_, _, n)| *n),
        "ended": s.collab_ended(),
    }))
}
