//! Shared-session commands (the fork's multiplayer work): join a session server, leave it, and
//! list who is in it; and cloud files, which are the server's sessions by name: list them, open
//! one (join it in a new tab) and save the active drawing as a new one. Ordinary commands, so the
//! CLI, the control channel and MCP can use them too.

#[cfg(not(target_arch = "wasm32"))]
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::{bad, str_param};
use crate::{CommandSpec, EngineError, Result, Session};

/// How long `collab.join` waits for the server's welcome.
#[cfg(not(target_arch = "wasm32"))]
const JOIN_WAIT: Duration = Duration::from_secs(20);
/// How long `cloud.list` waits for the list.
#[cfg(not(target_arch = "wasm32"))]
const LIST_WAIT: Duration = Duration::from_secs(10);

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("collab.join", "Join Shared Session", run_join)
            .params("{url: ws:// or wss:// session address, name?: shown to others, host?: bool (start the session with this drawing)} → {client, seq, peers}")
            .noundo(),
        CommandSpec::new("collab.leave", "Leave Shared Session", run_leave).params("{} → {left}").noundo(),
        CommandSpec::new("collab.peers", "Shared Session Status", run_peers)
            .params("{} → {live, joined, client?, peers: [{client, name}], pending, ended?}")
            .noundo(),
        CommandSpec::new("cloud.list", "List Cloud Files", run_cloud_list)
            .params("{server?: address (default $CADCRAFT_CLOUD or ws://127.0.0.1:9010)} → {server, files: [{name, modified: seconds since 1970, people: [name]}]}")
            .noundo(),
        CommandSpec::new("cloud.open", "Open Cloud File", run_cloud_open)
            .params("{name, server?, user?: shown to others} → {name, client, peers} (opens in a new tab; a name not in the cloud starts a new file)")
            .noundo(),
        CommandSpec::new("cloud.save", "Save to Cloud", run_cloud_save)
            .params("{name, server?, user?} → {name, client, peers} (the active drawing becomes a new cloud file; refused if the name is taken)")
            .noundo(),
    ]
}

/// The cloud server's address from `server`, `$CADCRAFT_CLOUD` or the local default, as
/// `ws://` or `wss://` without a trailing `/` (`http`/`https` and bare host names are accepted).
pub fn cloud_server(p: &Value) -> String {
    let raw =
        str_param(p, "server").map(str::to_string).or_else(|| std::env::var("CADCRAFT_CLOUD").ok()).unwrap_or_else(|| "ws://127.0.0.1:9010".into());
    let raw = raw.trim().trim_end_matches('/');
    if let Some(rest) = raw.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = raw.strip_prefix("http://") {
        format!("ws://{rest}")
    } else if raw.starts_with("ws://") || raw.starts_with("wss://") {
        raw.to_string()
    } else {
        format!("ws://{raw}")
    }
}

/// A cloud file name: 1 to 64 letters, digits, `-` and `_` (the server's session names).
fn cloud_name<'a>(cmd: &str, p: &'a Value) -> Result<&'a str> {
    let name = str_param(p, "name").map(str::trim).ok_or_else(|| bad(cmd, "name is required"))?;
    let ok = !name.is_empty() && name.len() <= 64 && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok { Ok(name) } else { Err(bad(cmd, "a cloud file name is 1 to 64 letters, digits, - and _")) }
}

fn cloud_user(p: &Value) -> String {
    str_param(p, "user").map(str::to_string).or_else(|| std::env::var("USER").ok()).unwrap_or_else(|| "CADCraft user".into())
}

#[cfg(target_arch = "wasm32")]
fn run_cloud_list(_: &mut Session, p: &Value) -> Result<Value> {
    Err(bad("cloud.list", format!("open {} in the browser for the file list", cloud_server(p).replacen("ws", "http", 1))))
}

#[cfg(not(target_arch = "wasm32"))]
fn run_cloud_list(_: &mut Session, p: &Value) -> Result<Value> {
    use cadcraft_collab::message::{ClientMsg, ServerMsg};
    let server = cloud_server(p);
    let mut link = cadcraft_collab_net::connect(&format!("{server}/files")).map_err(|e| bad("cloud.list", e.to_string()))?;
    link.send(&ClientMsg::List).map_err(|e| bad("cloud.list", e.to_string()))?;
    let start = Instant::now();
    loop {
        match link.recv() {
            Some(Ok(ServerMsg::Files { files })) => {
                let files: Vec<Value> = files.iter().map(|f| json!({"name": f.name, "modified": f.modified, "people": f.people})).collect();
                return Ok(json!({ "server": server, "files": files }));
            }
            Some(Ok(ServerMsg::Error { message })) => return Err(bad("cloud.list", message)),
            Some(Ok(_)) => {}
            Some(Err(e)) => return Err(bad("cloud.list", e.to_string())),
            None if !link.is_open() => return Err(bad("cloud.list", "the server closed the connection")),
            None => {}
        }
        if start.elapsed() > LIST_WAIT {
            return Err(bad("cloud.list", "the server did not answer in time"));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn run_cloud_open(s: &mut Session, p: &Value) -> Result<Value> {
    let name = cloud_name("cloud.open", p)?;
    let url = format!("{}/s/{name}", cloud_server(p));
    if s.collab_url() == Some(url.as_str()) {
        if let Some(i) = s.collab_uid().and_then(|uid| s.docs.iter().position(|d| d.uid == uid)) {
            s.active = i;
        }
        return cloud_result(s, name);
    }
    let link = cadcraft_collab_net::connect(&url).map_err(|e| bad("cloud.open", e.to_string()))?;
    // One shared drawing at a time: the one open before stays in its tab as a local copy.
    if s.collab_live() {
        s.collab_leave();
        s.echo("Left the previous cloud file; its tab is now a local copy.".to_string());
    }
    s.open_drawing(cadcraft_doc::Drawing::new_metric(), name, None);
    s.collab_attach_to(link, &url, &cloud_user(p), true, false)?;
    wait_for_welcome(s)?;
    #[cfg(not(target_arch = "wasm32"))]
    let _ = s.execute("zoom", &json!({"mode": "extents"}));
    cloud_result(s, name)
}

fn run_cloud_save(s: &mut Session, p: &Value) -> Result<Value> {
    let name = cloud_name("cloud.save", p)?;
    if s.collab_live() {
        return Err(bad("cloud.save", "this drawing is already shared; leave it first (collab.leave)"));
    }
    let url = format!("{}/s/{name}", cloud_server(p));
    let link = cadcraft_collab_net::connect(&url).map_err(|e| bad("cloud.save", e.to_string()))?;
    s.collab_attach_to(link, &url, &cloud_user(p), true, true)?;
    wait_for_welcome(s)?;
    s.state_mut()?.title = name.to_string();
    cloud_result(s, name)
}

fn cloud_result(s: &mut Session, name: &str) -> Result<Value> {
    let mut r = run_peers(s, &json!({}))?;
    r["name"] = json!(name);
    Ok(r)
}

fn run_join(s: &mut Session, p: &Value) -> Result<Value> {
    let url = str_param(p, "url").ok_or_else(|| bad("collab.join", "url is required"))?;
    if !(url.starts_with("ws://") || url.starts_with("wss://")) {
        return Err(bad("collab.join", "the url must start with ws:// or wss://"));
    }
    let name = str_param(p, "name").unwrap_or("CADCraft user");
    let host = p.get("host").and_then(Value::as_bool).unwrap_or(false);
    let link = cadcraft_collab_net::connect(url).map_err(|e| EngineError::Other(e.to_string()))?;
    s.collab_attach(link, name, host)?;
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

fn run_peers(s: &mut Session, _: &Value) -> Result<Value> {
    let status = s.collab_status();
    let peers: Vec<Value> =
        status.as_ref().map(|(_, p, _)| p.iter().map(|p| json!({"client": p.client, "name": p.name})).collect()).unwrap_or_default();
    Ok(json!({
        "live": s.collab_live(),
        "joined": s.collab_joined(),
        "client": status.as_ref().map(|(c, _, _)| *c),
        "peers": peers,
        "pending": status.as_ref().map_or(0, |(_, _, n)| *n),
        "ended": s.collab_ended(),
    }))
}
