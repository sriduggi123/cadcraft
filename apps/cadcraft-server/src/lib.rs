//! The CADCraft session server: shared drawing sessions over WebSockets. A client connects to
//! `/s/<session>`; the first to join a session brings its drawing, later ones receive it. Each
//! session's [`Server`] state machine orders the batches; this crate only moves bytes.
//!
//! P1 scope: sessions live in memory and end when the last person leaves; no accounts, no TLS
//! (run it behind a TLS-terminating proxy for `wss://`). The journal, persistence and accounts
//! are P2 and P6 of `plan/multiplayer-design.md`.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use cadcraft_collab::message::{self, ClientMsg, MAX_MESSAGE_BYTES, ServerMsg};
use cadcraft_collab::server::{Outgoing, Server, To};
use tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tungstenite::http::StatusCode;
use tungstenite::protocol::WebSocketConfig;
use tungstenite::{Message, WebSocket};

/// Most sessions at once.
pub const MAX_SESSIONS: usize = 1000;
/// Most people in one session.
pub const MAX_PEOPLE: usize = 64;
/// How long a read waits before the connection's thread sends what is queued.
const POLL: Duration = Duration::from_millis(15);

struct Room {
    server: Server,
    conns: HashMap<u32, Sender<Vec<u8>>>,
}

#[derive(Clone, Default)]
pub struct Rooms(Arc<Mutex<HashMap<String, Room>>>);

impl Rooms {
    fn lock(&self) -> MutexGuard<'_, HashMap<String, Room>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// How many sessions are open and how many people are in them.
    pub fn counts(&self) -> (usize, usize) {
        let rooms = self.lock();
        (rooms.len(), rooms.values().map(|r| r.conns.len()).sum())
    }
}

/// Accept connections on `listener` until it fails, one thread each.
pub fn serve(listener: TcpListener, rooms: Rooms) -> std::io::Result<()> {
    for stream in listener.incoming() {
        let stream = stream?;
        let rooms = rooms.clone();
        let _ = std::thread::Builder::new().name("collab-conn".into()).spawn(move || {
            if let Err(e) = connection(stream, &rooms) {
                eprintln!("connection ended: {e}");
            }
        });
    }
    Ok(())
}

/// A session name from a request path `/s/<name>`: 1 to 64 letters, digits, `-` and `_`.
pub fn session_name(path: &str) -> Option<String> {
    let name = path.strip_prefix("/s/")?;
    let ok = !name.is_empty() && name.len() <= 64 && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    ok.then(|| name.to_string())
}

fn not_found() -> ErrorResponse {
    let mut r = ErrorResponse::new(Some("connect to /s/<session>".into()));
    *r.status_mut() = StatusCode::NOT_FOUND;
    r
}

// The handshake callback's error type is tungstenite's HTTP response, large by its design.
#[allow(clippy::result_large_err)]
fn connection(stream: TcpStream, rooms: &Rooms) -> Result<(), String> {
    let mut name = None;
    let config = WebSocketConfig::default().max_message_size(Some(MAX_MESSAGE_BYTES)).max_frame_size(Some(MAX_MESSAGE_BYTES));
    let callback = |req: &Request, resp: Response| match session_name(req.uri().path()) {
        Some(n) => {
            name = Some(n);
            Ok(resp)
        }
        None => Err(not_found()),
    };
    let mut ws = tungstenite::accept_hdr_with_config(stream, callback, Some(config)).map_err(|e| e.to_string())?;
    let name = name.ok_or("no session name")?;
    ws.get_mut().set_read_timeout(Some(POLL)).map_err(|e| e.to_string())?;
    let (tx, rx) = mpsc::channel();
    let mut client = None;
    let result = pump(&mut ws, &rx, |msg| handle(rooms, &name, &mut client, &tx, msg));
    if let Some(id) = client {
        let mut all = rooms.lock();
        if let Some(room) = all.get_mut(&name) {
            room.conns.remove(&id);
            let out = room.server.leave(id);
            route(room, out);
            if room.conns.is_empty() {
                all.remove(&name);
                eprintln!("session {name} closed");
            }
        }
    }
    result
}

/// Move messages until the client closes or breaks the protocol.
fn pump(ws: &mut WebSocket<TcpStream>, rx: &Receiver<Vec<u8>>, mut on: impl FnMut(ClientMsg) -> Result<(), String>) -> Result<(), String> {
    loop {
        loop {
            match rx.try_recv() {
                Ok(bytes) => ws.send(Message::Binary(bytes.into())).map_err(|e| e.to_string())?,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Ok(()),
            }
        }
        match ws.read() {
            Ok(Message::Binary(b)) => on(message::decode(&b).map_err(|e| e.to_string())?)?,
            Ok(Message::Close(_)) => return Ok(()),
            Ok(_) => {}
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                let _ = ws.flush();
            }
            Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => return Ok(()),
            Err(e) => return Err(e.to_string()),
        }
    }
}

fn handle(rooms: &Rooms, name: &str, client: &mut Option<u32>, tx: &Sender<Vec<u8>>, msg: ClientMsg) -> Result<(), String> {
    let mut all = rooms.lock();
    match (msg, *client) {
        (ClientMsg::Hello { name: who, seed }, None) => {
            if !all.contains_key(name) && all.len() >= MAX_SESSIONS {
                return refuse(tx, "the server is full");
            }
            let room = all.entry(name.to_string()).or_insert_with(|| Room { server: Server::new(None), conns: HashMap::new() });
            if room.conns.len() >= MAX_PEOPLE {
                return refuse(tx, "the session is full");
            }
            let (id, out) = match room.server.join(&who, seed.as_deref()) {
                Ok(j) => j,
                Err(e) => return refuse(tx, &e.to_string()),
            };
            room.conns.insert(id, tx.clone());
            *client = Some(id);
            eprintln!("session {name}: {who} joined as client {id}");
            route(room, out);
            Ok(())
        }
        (ClientMsg::Batch { batch, changes }, Some(id)) => {
            let room = all.get_mut(name).ok_or("the session is gone")?;
            let out = room.server.batch(id, batch, changes);
            route(room, out);
            Ok(())
        }
        _ => refuse(tx, "hello comes first, and once"),
    }
}

/// Tell the client why and end its connection.
fn refuse(tx: &Sender<Vec<u8>>, why: &str) -> Result<(), String> {
    if let Ok(bytes) = message::encode(&ServerMsg::Error { message: why.to_string() }) {
        let _ = tx.send(bytes);
    }
    Err(why.to_string())
}

fn route(room: &Room, out: Vec<Outgoing>) {
    for o in out {
        let Ok(bytes) = message::encode(&o.msg) else { continue };
        for (id, tx) in &room.conns {
            if o.to == To::All || o.to == To::One(*id) {
                let _ = tx.send(bytes.clone());
            }
        }
    }
}
