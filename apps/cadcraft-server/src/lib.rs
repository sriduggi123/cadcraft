//! The CADCraft session server: cloud files edited together over WebSockets. A client makes a
//! file on `/new` with its drawing; the server names it with 128 random bits, and its link
//! (`/s/<id>`) is the key to it: joining needs an existing id, and nothing lists the files
//! (`/status` answers only about ids it is given). Each file's [`Server`] state machine orders
//! the batches; this crate moves bytes, serves the web app and DWG/DXF downloads on the same port
//! (`http.rs`), and keeps the files on disk.
//!
//! With a data folder, each file's drawing is written there (when made, [`SAVE_EVERY`] at most
//! while edited, and when the last person leaves) with its title beside it, and reopens from it.
//! No accounts and no TLS (run it behind a TLS-terminating proxy for `wss://`); accounts are P6 of
//! `plan/multiplayer-design.md`.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![forbid(unsafe_code)]

pub mod http;

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use cadcraft_collab::message::{self, ClientMsg, FileInfo, MAX_MESSAGE_BYTES, ServerMsg};
use cadcraft_collab::server::{Outgoing, Server};
use cadcraft_collab::snapshot;
use cadcraft_doc::Drawing;
use tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tungstenite::http::StatusCode;
use tungstenite::protocol::WebSocketConfig;
use tungstenite::{Message, WebSocket};

/// Most sessions at once.
pub const MAX_SESSIONS: usize = 1000;
/// Most people in one session.
pub const MAX_PEOPLE: usize = 64;
/// How often changed sessions are written to the data folder.
pub const SAVE_EVERY: Duration = Duration::from_secs(2);
/// Most files one status question asks about.
pub const MAX_STATUS: usize = 500;
/// How long a read waits before the connection's thread sends what is queued.
const POLL: Duration = Duration::from_millis(15);

struct Room {
    server: Server,
    conns: HashMap<u32, Sender<Vec<u8>>>,
    /// The server's `seq` last written to disk (`None`: not written yet).
    saved: Option<u64>,
}

/// Where the server keeps things: the built web app (`dist/web`) and the sessions' drawings.
#[derive(Clone, Debug, Default)]
pub struct Config {
    pub web: Option<PathBuf>,
    pub data: Option<PathBuf>,
}

#[derive(Clone, Default)]
pub struct Rooms {
    rooms: Arc<Mutex<HashMap<String, Room>>>,
    config: Arc<Config>,
}

impl Rooms {
    pub fn new(config: Config) -> Rooms {
        Rooms { rooms: Arc::default(), config: Arc::new(config) }
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, Room>> {
        self.rooms.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// How many sessions are open and how many people are in them.
    pub fn counts(&self) -> (usize, usize) {
        let rooms = self.lock();
        (rooms.len(), rooms.values().map(|r| r.conns.len()).sum())
    }

    /// How the files with these ids are doing (the ones that exist; at most [`MAX_STATUS`]).
    /// Only ids the asker already knows are answered: nobody can list the server's files.
    pub fn status(&self, ids: &[String]) -> Vec<FileInfo> {
        let mut out = Vec::new();
        for id in ids.iter().take(MAX_STATUS) {
            let Some(name) = session_name(&format!("/s/{id}")) else { continue };
            let live =
                self.lock().get(&name).map(|r| (r.server.title().to_string(), r.server.peers().into_iter().map(|p| p.name).collect::<Vec<_>>()));
            let saved = self.file(&name).and_then(|p| p.metadata().ok()).and_then(|m| m.modified().ok()).map(seconds);
            let (title, people) = match (live, saved) {
                (Some(l), _) => l,
                (None, Some(_)) => (self.saved_title(&name), Vec::new()),
                (None, None) => continue,
            };
            let modified = if people.is_empty() { saved.unwrap_or(0) } else { seconds(std::time::SystemTime::now()) };
            out.push(FileInfo { name, title, modified, people });
        }
        out
    }

    /// Whether a cloud file `name` exists, live or saved.
    fn exists(&self, name: &str) -> bool {
        self.lock().contains_key(name) || self.file(name).is_some_and(|p| p.exists())
    }

    /// The title saved next to file `name`'s drawing.
    fn saved_title(&self, name: &str) -> String {
        let path = self.config.data.as_ref().map(|d| d.join(format!("{name}.title")));
        path.and_then(|p| std::fs::read_to_string(p).ok())
            .map(|t| t.trim().chars().take(cadcraft_collab::server::MAX_TITLE).collect())
            .unwrap_or_default()
    }

    fn save_title(&self, name: &str, title: &str) {
        if let Some(d) = &self.config.data
            && let Err(e) = std::fs::write(d.join(format!("{name}.title")), title)
        {
            eprintln!("file {name}: title not saved: {e}");
        }
    }

    /// The session's file in the data folder.
    fn file(&self, name: &str) -> Option<PathBuf> {
        self.config.data.as_ref().map(|d| d.join(format!("{name}.cadsession")))
    }

    /// The drawing saved for session `name`, if there is one. An unreadable file is an error, so
    /// a new session never replaces it.
    fn load(&self, name: &str) -> Result<Option<Drawing>, String> {
        let Some(path) = self.file(name).filter(|p| p.exists()) else { return Ok(None) };
        let bytes = std::fs::read(&path).map_err(|e| format!("can't read {}: {e}", path.display()))?;
        snapshot::decode(&bytes).map(Some).map_err(|e| format!("the saved session {name} could not be read: {e}"))
    }

    /// The session's drawing now: live, or as last saved.
    pub fn drawing(&self, name: &str) -> Result<Option<Drawing>, String> {
        if let Some(d) = self.lock().get(name).and_then(|r| r.server.drawing().cloned()) {
            return Ok(Some(d));
        }
        self.load(name)
    }

    /// Write every session changed since its last save.
    pub fn save_changed(&self) {
        let changed: Vec<(String, u64, Drawing)> = {
            let rooms = self.lock();
            rooms
                .iter()
                .filter(|(_, r)| r.saved != Some(r.server.seq()))
                .filter_map(|(n, r)| Some((n.clone(), r.server.seq(), r.server.drawing()?.clone())))
                .collect()
        };
        for (name, seq, d) in changed {
            if self.save(&name, &d)
                && let Some(r) = self.lock().get_mut(&name)
            {
                r.saved = Some(seq);
            }
        }
    }

    /// Write `d` as session `name`'s file (through a temporary file, so a crash never leaves half
    /// a file). Whether it was written.
    fn save(&self, name: &str, d: &Drawing) -> bool {
        let Some(path) = self.file(name) else { return false };
        let result = snapshot::encode(d).map_err(|e| e.to_string()).and_then(|bytes| write_replacing(&path, &bytes).map_err(|e| e.to_string()));
        if let Err(e) = &result {
            eprintln!("session {name}: not saved: {e}");
        }
        result.is_ok()
    }
}

fn seconds(t: std::time::SystemTime) -> u64 {
    t.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn write_replacing(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("cadsession.tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

/// Accept connections on `listener` until it fails, one thread each; with a data folder, a
/// thread saves changed sessions every [`SAVE_EVERY`].
pub fn serve(listener: TcpListener, rooms: Rooms) -> std::io::Result<()> {
    if let Some(dir) = &rooms.config.data {
        std::fs::create_dir_all(dir)?;
        let saver = rooms.clone();
        std::thread::Builder::new().name("collab-save".into()).spawn(move || {
            loop {
                std::thread::sleep(SAVE_EVERY);
                saver.save_changed();
            }
        })?;
    }
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

/// The connection's stream with the request head, already read to tell a WebSocket from a page
/// request, put back in front.
struct Replay {
    head: Vec<u8>,
    pos: usize,
    inner: TcpStream,
}

impl Read for Replay {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let rest = self.head.get(self.pos..).unwrap_or_default();
        if rest.is_empty() {
            return self.inner.read(buf);
        }
        let n = rest.len().min(buf.len());
        buf[..n].copy_from_slice(&rest[..n]);
        self.pos += n;
        Ok(n)
    }
}

impl Write for Replay {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.inner.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

fn not_found() -> ErrorResponse {
    let mut r = ErrorResponse::new(Some("connect to /s/<session>".into()));
    *r.status_mut() = StatusCode::NOT_FOUND;
    r
}

// The handshake callback's error type is tungstenite's HTTP response, large by its design.
#[allow(clippy::result_large_err)]
fn connection(mut stream: TcpStream, rooms: &Rooms) -> Result<(), String> {
    let head = http::read_head(&mut stream).map_err(|e| e.to_string())?;
    if !http::is_websocket(&head) {
        return http::respond(&mut stream, &head, rooms).map_err(|e| e.to_string());
    }
    let mut name = None;
    let config = WebSocketConfig::default().max_message_size(Some(MAX_MESSAGE_BYTES)).max_frame_size(Some(MAX_MESSAGE_BYTES));
    // `/new` makes a cloud file and `/status` answers about files by id; neither is a name a file
    // can have.
    let callback = |req: &Request, resp: Response| match session_name(req.uri().path())
        .or_else(|| matches!(req.uri().path(), NEW | STATUS).then(|| req.uri().path().to_string()))
    {
        Some(n) => {
            name = Some(n);
            Ok(resp)
        }
        None => Err(not_found()),
    };
    let replay = Replay { head, pos: 0, inner: stream };
    let mut ws = tungstenite::accept_hdr_with_config(replay, callback, Some(config)).map_err(|e| e.to_string())?;
    let mut name = name.ok_or("no session name")?;
    ws.get_mut().inner.set_read_timeout(Some(POLL)).map_err(|e| e.to_string())?;
    let (tx, rx) = mpsc::channel();
    let mut client = None;
    let result = pump(&mut ws, &rx, |msg| handle(rooms, &mut name, &mut client, &tx, msg));
    if let Some(id) = client {
        let mut all = rooms.lock();
        if let Some(room) = all.get_mut(&name) {
            room.conns.remove(&id);
            let out = room.server.leave(id);
            route(room, out);
            if room.conns.is_empty() {
                let last = room.server.drawing().filter(|_| room.saved != Some(room.server.seq())).cloned();
                all.remove(&name);
                drop(all);
                if let Some(d) = last {
                    rooms.save(&name, &d);
                }
                eprintln!("session {name} closed");
            }
        }
    }
    result
}

/// Move messages until the client closes or breaks the protocol.
fn pump(ws: &mut WebSocket<Replay>, rx: &Receiver<Vec<u8>>, mut on: impl FnMut(ClientMsg) -> Result<(), String>) -> Result<(), String> {
    loop {
        loop {
            match rx.try_recv() {
                Ok(bytes) => ws.send(Message::Binary(bytes.into())).map_err(|e| e.to_string())?,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Ok(()),
            }
        }
        match ws.read() {
            Ok(Message::Binary(b)) => {
                if let Err(e) = message::decode(&b).map_err(|e| e.to_string()).and_then(&mut on) {
                    // Send what was queued (the refusal's reason) before closing.
                    while let Ok(bytes) = rx.try_recv() {
                        let _ = ws.send(Message::Binary(bytes.into()));
                    }
                    let _ = ws.close(None);
                    let _ = ws.flush();
                    return Err(e);
                }
            }
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

/// The path that makes a new cloud file, and the one answering about files by id.
const NEW: &str = "/new";
const STATUS: &str = "/status";

/// A new file id: 128 random bits as 22 URL-safe characters. The link is the key to the file.
fn new_id() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|e| format!("no random numbers: {e}"))?;
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut bits = u128::from_le_bytes(bytes);
    let mut id = String::with_capacity(22);
    for _ in 0..22 {
        id.push(char::from(ABC[(bits & 63) as usize]));
        bits >>= 6;
    }
    Ok(id)
}

fn handle(rooms: &Rooms, name: &mut String, client: &mut Option<u32>, tx: &Sender<Vec<u8>>, msg: ClientMsg) -> Result<(), String> {
    match (msg, *client) {
        (ClientMsg::List { files }, None) if name == STATUS => {
            if let Ok(bytes) = message::encode(&ServerMsg::Files { files: rooms.status(&files) }) {
                let _ = tx.send(bytes);
            }
            Ok(())
        }
        (ClientMsg::Hello { .. }, None) if name == STATUS => refuse(tx, "open a file by its link"),
        (ClientMsg::Hello { name: who, seed, title, .. }, None) if name == NEW => {
            let Some(seed) = seed else { return refuse(tx, "a new file needs a drawing") };
            let id = new_id()?;
            let title: String = title.unwrap_or_default().trim().chars().take(cadcraft_collab::server::MAX_TITLE).collect();
            let mut all = rooms.lock();
            if all.len() >= MAX_SESSIONS {
                return refuse(tx, "the server is full");
            }
            let mut server = Server::new(None);
            server.set_file(&id, &title);
            let (client_id, out) = match server.join(&who, Some(&seed), None) {
                Ok(j) => j,
                Err(e) => return refuse(tx, &e.to_string()),
            };
            let mut room = Room { server, conns: HashMap::new(), saved: None };
            room.conns.insert(client_id, tx.clone());
            route(&room, out);
            let first = room.server.drawing().cloned();
            all.insert(id.clone(), room);
            drop(all);
            // On disk at once, so the link works even if the server stops in the next seconds.
            rooms.save_title(&id, &title);
            if let Some(d) = first {
                rooms.save(&id, &d);
            }
            *client = Some(client_id);
            eprintln!("file {id} ({title}) made by {who}");
            *name = id;
            Ok(())
        }
        (ClientMsg::Hello { name: who, resume, .. }, None) => {
            if !rooms.exists(name) {
                return refuse(tx, "there is no file with this link (check it was copied whole)");
            }
            // Read a saved session before taking the lock; it is used only if the room is new.
            let saved = if rooms.lock().contains_key(name.as_str()) {
                None
            } else {
                match rooms.load(name) {
                    Ok(d) => d,
                    Err(e) => return refuse(tx, &e),
                }
            };
            let title = rooms.saved_title(name);
            let mut all = rooms.lock();
            if !all.contains_key(name.as_str()) && all.len() >= MAX_SESSIONS {
                return refuse(tx, "the server is full");
            }
            let room = all.entry(name.to_string()).or_insert_with(|| {
                let saved_seq = saved.is_some().then_some(0);
                let mut server = Server::new(saved);
                server.set_file(name, &title);
                Room { server, conns: HashMap::new(), saved: saved_seq }
            });
            if room.conns.len() >= MAX_PEOPLE {
                return refuse(tx, "the session is full");
            }
            let (id, out) = match room.server.join(&who, None, resume) {
                Ok(j) => j,
                Err(e) => return refuse(tx, &e.to_string()),
            };
            room.conns.insert(id, tx.clone());
            *client = Some(id);
            eprintln!("file {name}: {who} joined as client {id}");
            route(room, out);
            Ok(())
        }
        (ClientMsg::Batch { batch, base, changes }, Some(id)) => {
            let mut all = rooms.lock();
            let room = all.get_mut(name.as_str()).ok_or("the session is gone")?;
            let out = room.server.batch(id, batch, base, changes);
            route(room, out);
            Ok(())
        }
        (ClientMsg::Lock { block, on }, Some(id)) => {
            let mut all = rooms.lock();
            let room = all.get_mut(name.as_str()).ok_or("the session is gone")?;
            let out = room.server.lock(id, &block, on);
            route(room, out);
            Ok(())
        }
        (ClientMsg::Presence(p), Some(id)) => {
            let mut all = rooms.lock();
            let room = all.get_mut(name.as_str()).ok_or("the session is gone")?;
            let out = room.server.presence(id, p);
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
            if o.to.reaches(*id) {
                let _ = tx.send(bytes.clone());
            }
        }
    }
}
