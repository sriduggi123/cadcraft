//! Shared-session commands (the fork's multiplayer work): join a session server, leave it, and
//! list who is in it. Ordinary commands, so the CLI, the control channel and MCP can join too.

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::{bad, str_param};
use crate::{CommandSpec, EngineError, Result, Session};

/// How long `collab.join` waits for the server's welcome.
const JOIN_WAIT: Duration = Duration::from_secs(20);

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("collab.join", "Join Shared Session", run_join)
            .params("{url: ws:// or wss:// session address, name?: shown to others, host?: bool (start the session with this drawing)} → {client, seq, peers}")
            .noundo(),
        CommandSpec::new("collab.leave", "Leave Shared Session", run_leave).params("{} → {left}").noundo(),
        CommandSpec::new("collab.peers", "Shared Session Status", run_peers)
            .params("{} → {live, joined, client?, peers: [{client, name}], pending, ended?}")
            .noundo(),
    ]
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
    run_peers(s, &json!({}))
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
