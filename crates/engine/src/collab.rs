//! Shared sessions (the fork's multiplayer work, `plan/multiplayer-design.md`): one open drawing
//! is edited by several people through a session server. After each command that changed it, the
//! drawing's changes go to the server ([`Session::collab_after_edit`]); between commands,
//! [`Session::collab_tick`] applies what the server sent. Nothing arrives while a command runs, so
//! a command never sees the drawing change under it.
//!
//! Undo in a session takes back only this person's steps ([`DocState::revert_in`]): swapping the
//! drawing back would take back other people's edits too. The Block Editor and REFEDIT lock their
//! block on the server while open. REFEDIT edits the block in place in the drawing, so while it
//! lasts this person's changes and others' arriving edits wait; REFCLOSE sends the result and
//! then applies what arrived.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError};

use web_time::{Duration, Instant};

pub use cadcraft_collab::Link;
use cadcraft_collab::client::Client;
use cadcraft_collab::message::{ClientMsg, MAX_SELECTION, Peer, Presence, ServerMsg};
use cadcraft_collab::snapshot;
use cadcraft_doc::{Drawing, Handle, Space};
use cadcraft_geom::Vec2;

use crate::view::View;
use crate::{EngineError, Result, Session};

/// Most messages applied in one tick, so a burst doesn't stall a frame.
const MAX_PER_TICK: usize = 256;

/// How long a dropped connection is tried again before the session ends, and the longest wait
/// between tries.
const RECONNECT_FOR: Duration = Duration::from_secs(120);
const RECONNECT_MAX_WAIT: Duration = Duration::from_secs(15);

/// Least time between two presence messages, in seconds (20 a second at most).
const PRESENCE_EVERY: f64 = 0.05;

/// Commands that can't run in a shared session (none at present).
const REFUSED: &[&str] = &[];

/// The live session of one drawing.
pub struct Live {
    link: Box<dyn Link>,
    /// The drawing (its [`crate::DocState::uid`]) shared.
    uid: u64,
    /// `None` until the server's welcome arrives.
    client: Option<Client>,
    /// Why the session ended, when it did.
    ended: Option<String>,
    /// The session's address (`ws://host/s/<name>`), when known.
    url: String,
    /// The presence last sent, and when (the caller's clock, in seconds).
    sent: Option<Presence>,
    sent_at: f64,
    /// The person whose view this one follows, and the view last copied from them.
    following: Option<u32>,
    followed: Option<View>,
    /// The shared drawing as last sent or received: the next batch is what changed since. While
    /// the Block Editor is open it is the drawing waiting behind the editor, which changes only
    /// when the block is saved.
    known: Option<Arc<Drawing>>,
    /// The block this person has locked for the Block Editor.
    locked: Option<String>,
    /// The cloud file's title.
    title: String,
    /// The name shown to others.
    name: String,
    /// The connection dropped and is being made again.
    retry: Option<Retry>,
    /// A new connection sent its hello: the next welcome is a rejoin.
    rejoining: bool,
}

/// Connecting again after the connection dropped.
struct Retry {
    since: Instant,
    next: Instant,
    tries: u32,
    connecting: Option<Receiver<std::result::Result<Box<dyn Link>, String>>>,
}

/// Connect to `url` without waiting: natively on a thread, in the browser at once (its WebSocket
/// opens in the background).
fn connect_later(url: &str) -> Receiver<std::result::Result<Box<dyn Link>, String>> {
    let (tx, rx) = std::sync::mpsc::channel();
    let url = url.to_string();
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = std::thread::Builder::new().name("collab-reconnect".into()).spawn(move || {
            let _ = tx.send(cadcraft_collab_net::connect(&url).map_err(|e| e.to_string()));
        });
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = tx.send(cadcraft_collab_net::connect(&url).map_err(|e| e.to_string()));
    }
    rx
}

impl Session {
    /// Join a session over `link` with the active drawing. `host`: start the session with this
    /// drawing (ignored if the session already has one). The welcome arrives through
    /// [`Session::collab_tick`].
    pub fn collab_attach(&mut self, link: Box<dyn Link>, name: &str, host: bool) -> Result<()> {
        self.collab_attach_to(link, "", name, host.then_some(""))
    }

    /// [`Session::collab_attach`] to the session at `url` (`…/s/<file>`, or `…/new` to make a
    /// new cloud file). `new`: start it with the active drawing under this title.
    pub fn collab_attach_to(&mut self, link: Box<dyn Link>, url: &str, name: &str, new: Option<&str>) -> Result<()> {
        if self.collab.as_ref().is_some_and(|l| l.ended.is_none()) {
            return Err(EngineError::Other("already in a shared session (collab.leave first)".into()));
        }
        let st = self.state()?;
        let seed = if new.is_some() { Some(snapshot::encode(&st.doc).map_err(|e| EngineError::Other(e.to_string()))?) } else { None };
        let uid = st.uid;
        let mut link = link;
        let hello = ClientMsg::Hello { name: name.to_string(), seed, title: new.map(str::to_string), resume: None };
        link.send(&hello).map_err(|e| EngineError::Other(e.to_string()))?;
        self.collab = Some(Live {
            link,
            uid,
            client: None,
            ended: None,
            url: url.to_string(),
            sent: None,
            sent_at: 0.0,
            following: None,
            followed: None,
            known: None,
            locked: None,
            title: String::new(),
            name: name.to_string(),
            retry: None,
            rejoining: false,
        });
        Ok(())
    }

    /// The cloud file's title, once joined.
    pub fn collab_title(&self) -> Option<&str> {
        self.collab.as_ref().filter(|l| l.ended.is_none() && l.client.is_some()).map(|l| l.title.as_str())
    }

    /// The address of the session attached, if one is.
    pub fn collab_url(&self) -> Option<&str> {
        self.collab.as_ref().filter(|l| l.ended.is_none()).map(|l| l.url.as_str()).filter(|u| !u.is_empty())
    }

    /// The drawing (its uid) the session shares.
    pub fn collab_uid(&self) -> Option<u64> {
        self.collab.as_ref().filter(|l| l.ended.is_none()).map(|l| l.uid)
    }

    /// Whether a session is attached and hasn't ended.
    pub fn collab_live(&self) -> bool {
        self.collab.as_ref().is_some_and(|l| l.ended.is_none())
    }

    /// Whether the session's welcome has arrived.
    pub fn collab_joined(&self) -> bool {
        self.collab.as_ref().is_some_and(|l| l.client.is_some() && l.ended.is_none())
    }

    /// Why the session ended, if it did.
    pub fn collab_ended(&self) -> Option<&str> {
        self.collab.as_ref().and_then(|l| l.ended.as_deref())
    }

    /// The people in the session, this client's id and its unconfirmed batches.
    pub fn collab_status(&self) -> Option<(u32, Vec<Peer>, usize)> {
        let c = self.collab.as_ref()?.client.as_ref()?;
        Some((c.id(), c.peers().to_vec(), c.pending()))
    }

    /// Leave the session; the drawing stays as it is.
    pub fn collab_leave(&mut self) {
        self.collab = None;
    }

    /// Whether the shared drawing has a block open for editing in place (REFEDIT).
    fn collab_editing_in_place(&self) -> bool {
        self.collab.as_ref().is_some_and(|l| self.docs.iter().any(|d| d.uid == l.uid && d.doc.refedit.is_some()))
    }

    /// Whether the active drawing is the shared one.
    pub(crate) fn collab_shares_active(&self) -> bool {
        self.collab_live() && self.state().is_ok_and(|st| self.collab.as_ref().is_some_and(|l| l.uid == st.uid))
    }

    /// Tell the person what an undo left because someone changed it since.
    pub(crate) fn note_left(&mut self, left: usize) {
        if left > 0 {
            let what = if left == 1 { "1 change was".to_string() } else { format!("{left} changes were") };
            self.echo(format!("{what} left as they are: someone else changed the same thing since."));
        }
    }

    /// Whether `id` is refused while the active drawing is shared.
    pub(crate) fn collab_refuses(&self, id: &str) -> Option<String> {
        (self.collab_shares_active() && REFUSED.contains(&id)).then(|| format!("{} isn't available in a shared session yet", id.to_ascii_uppercase()))
    }

    /// Apply what the server sent. Call between commands (the UI calls it every frame).
    pub fn collab_tick(&mut self) {
        if self.running.is_some() || self.lisp_busy() {
            return;
        }
        if self.collab.as_ref().is_some_and(|l| l.retry.is_some()) {
            return self.collab_retry();
        }
        self.collab_sync_lock();
        // Others' edits wait while a block is edited in place (they would rebuild the drawing
        // under the working set); REFCLOSE lets them in.
        if self.collab_editing_in_place() {
            return;
        }
        self.collab_drain();
        self.collab_follow_step();
    }

    /// Whether the connection dropped and is being made again.
    pub fn collab_reconnecting(&self) -> bool {
        self.collab.as_ref().is_some_and(|l| l.ended.is_none() && (l.retry.is_some() || l.rejoining))
    }

    /// Try the dropped connection again: one try at a time, waiting 1, 2, 4… up to 15 seconds
    /// between them, for two minutes.
    fn collab_retry(&mut self) {
        let Some(live) = self.collab.as_mut() else { return };
        let Some(retry) = live.retry.as_mut() else { return };
        let now = Instant::now();
        if let Some(rx) = &retry.connecting {
            let result = match rx.try_recv() {
                Ok(r) => r,
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => Err("the connection attempt stopped".into()),
            };
            retry.connecting = None;
            if let Ok(link) = result {
                let id = live.client.as_ref().map(Client::id);
                live.link = link;
                let hello = ClientMsg::Hello { name: live.name.clone(), seed: None, title: None, resume: id };
                if live.link.send(&hello).is_ok() {
                    live.retry = None;
                    live.rejoining = true;
                    return;
                }
            }
            let Some(retry) = live.retry.as_mut() else { return };
            retry.tries += 1;
            retry.next = now + Duration::from_secs(1u64 << retry.tries.min(4)).min(RECONNECT_MAX_WAIT);
            return;
        }
        if now.duration_since(retry.since) > RECONNECT_FOR {
            return self.collab_end("the connection to the server was lost and could not be made again".into());
        }
        if now >= retry.next {
            retry.connecting = Some(connect_later(&live.url));
        }
    }

    fn collab_drain(&mut self) {
        let start = self.collab.as_ref().and_then(|l| self.docs.iter().find(|d| d.uid == l.uid)).map(|d| d.doc.clone());
        self.collab_drain_messages();
        if let Some(start) = start {
            self.collab_repair(&start);
        }
    }

    /// Repair derived data after others' edits arrived (`start` is the drawing before they did):
    /// associative dimensions and hatches whose objects someone changed while another changed
    /// them, when they no longer agree, and objects on a layer that is gone (renamed meanwhile:
    /// they follow the rename; else the layer is made again). One person does it, the one with
    /// the lowest client id, and the fix goes out as an edit.
    fn collab_repair(&mut self, start: &Arc<Drawing>) {
        let pairs = self.collab.as_mut().and_then(|l| l.client.as_mut()).map(Client::take_concurrent).unwrap_or_default();
        let suspect = |own: Handle, sources: &[Handle]| {
            pairs.iter().any(|(a, b)| {
                (a.contains(&own) && sources.iter().any(|s| b.contains(s))) || (b.contains(&own) && sources.iter().any(|s| a.contains(s)))
            })
        };
        let Some(live) = self.collab.as_ref().filter(|l| l.ended.is_none()) else { return };
        let Some(client) = live.client.as_ref() else { return };
        if client.peers().iter().map(|p| p.client).min().is_some_and(|m| m != client.id()) {
            return;
        }
        let uid = live.uid;
        let Some(st) = self.docs.iter().find(|d| d.uid == uid) else { return };
        if Arc::ptr_eq(start, &st.doc) || st.block_edit.is_some() {
            return;
        }
        let pre = st.doc.clone();
        let mut d = pre.clone();
        let mut fixed = if pairs.is_empty() { 0 } else { crate::assoc::repair(&mut d, suspect) };
        if !pairs.is_empty() {
            fixed += crate::cmd::hatch::repair_associative(&mut d, suspect);
        }
        fixed += repair_references(&mut d, start, |table, name| client.renamed(table, name).map(str::to_string));
        if fixed == 0 || Arc::ptr_eq(&d, &pre) {
            return;
        }
        if let Some(st) = self.docs.iter_mut().find(|x| x.uid == uid) {
            st.doc = d;
            st.revision += 1;
        }
        self.collab_after_edit(&pre, uid, "repair");
    }

    fn collab_drain_messages(&mut self) {
        for _ in 0..MAX_PER_TICK {
            let Some(live) = self.collab.as_mut().filter(|l| l.ended.is_none()) else { return };
            let msg = match live.link.recv() {
                Some(Ok(m)) => m,
                Some(Err(e)) => return self.collab_end(format!("a message from the server could not be read: {e}")),
                None if !live.link.is_open() => {
                    // A joined session with an address tries again; one without ends.
                    if live.client.is_some() && !live.url.is_empty() {
                        // The first try waits a second, so the server has seen the drop and
                        // gives this person their id back.
                        let now = Instant::now();
                        live.retry = Some(Retry { since: now, next: now + Duration::from_secs(1), tries: 0, connecting: None });
                        live.rejoining = false;
                        return self.echo("Connection to the server lost; reconnecting…");
                    }
                    return self.collab_end("the connection to the server closed".into());
                }
                None => return,
            };
            if let Err(e) = self.collab_receive(msg) {
                return self.collab_end(e);
            }
        }
    }

    /// Send where this person is: `cursor` in drawing coordinates while it is over the drawing,
    /// `now` the caller's clock in seconds. The selection, the command running and the view come
    /// from the session. Sent only when it changed, 20 times a second at most. The UI calls it
    /// every frame.
    pub fn collab_presence(&mut self, cursor: Option<Vec2>, now: f64) {
        let Some(live) = self.collab.as_ref().filter(|l| l.client.is_some() && l.ended.is_none() && l.retry.is_none() && !l.rejoining) else {
            return;
        };
        let uid = live.uid;
        let mut p = Presence::default();
        if let Some(st) = self.docs.get(self.active).filter(|d| d.uid == uid && d.block_edit.is_none()) {
            p.layout = layout_name(&st.space);
            p.cursor = cursor.filter(|c| c.is_finite()).map(|c| [c.x, c.y]);
            p.selection = st.selection.iter().take(MAX_SELECTION).copied().collect();
            p.command = self.running.as_ref().map(|r| r.id.to_ascii_uppercase());
            // The rubber band, unless inside a layout viewport (its model coordinates aren't the
            // sheet's, which the cursor is in).
            if let (Some(c), Some(_), None) = (cursor.filter(|c| c.is_finite()), &self.running, st.mspace) {
                let mut size = 0;
                for e in self.preview(c).iter().take(cadcraft_collab::message::MAX_PREVIEW) {
                    let Ok(j) = cadcraft_collab::Json::of(e) else { continue };
                    size += j.0.to_string().len();
                    if size > cadcraft_collab::message::MAX_PREVIEW_BYTES {
                        break;
                    }
                    p.preview.push(j);
                }
            }
            // Inside a layout viewport, the sheet's view: that is what others on the tab share.
            let v = if st.mspace.is_some() { st.paper_view() } else { st.view() };
            p.view = Some([v.center.x, v.center.y, v.height]);
        }
        let Some(live) = self.collab.as_mut() else { return };
        let due = now - live.sent_at >= PRESENCE_EVERY || now < live.sent_at;
        if live.sent.as_ref() == Some(&p) || !due {
            return;
        }
        live.sent_at = now;
        let sent = live.link.send(&ClientMsg::Presence(p.clone()));
        live.sent = Some(p);
        if let Err(e) = sent {
            self.collab_end(format!("the connection to the server failed: {e}"));
        }
    }

    /// The others in the session with where they are, by client id.
    pub fn collab_others(&self) -> Vec<(Peer, Presence)> {
        let Some(c) = self.collab.as_ref().filter(|l| l.ended.is_none()).and_then(|l| l.client.as_ref()) else { return Vec::new() };
        c.peers().iter().filter(|p| p.client != c.id()).map(|p| (p.clone(), c.others().get(&p.client).cloned().unwrap_or_default())).collect()
    }

    /// Follow `client`'s view (None: stop).
    pub fn collab_follow(&mut self, client: Option<u32>) {
        if let Some(l) = self.collab.as_mut() {
            l.following = client;
            l.followed = None;
        }
        self.collab_follow_step();
    }

    /// The person this one follows.
    pub fn collab_following(&self) -> Option<u32> {
        self.collab.as_ref().filter(|l| l.ended.is_none()).and_then(|l| l.following)
    }

    /// Copy the followed person's view; stop when they leave or this person pans or zooms.
    fn collab_follow_step(&mut self) {
        let Some(live) = self.collab.as_ref().filter(|l| l.ended.is_none()) else { return };
        let (Some(who), Some(client)) = (live.following, live.client.as_ref()) else { return };
        let name = client.peers().iter().find(|p| p.client == who).map(|p| p.name.clone());
        let lead = client.others().get(&who).cloned();
        let (uid, followed) = (live.uid, live.followed);
        let Some(name) = name else {
            self.collab_follow(None);
            return self.echo("Stopped following: they left the session.");
        };
        let Some(st) = self.docs.get_mut(self.active).filter(|d| d.uid == uid) else { return };
        if followed.is_some_and(|v| !same_view(&v, &st.view())) {
            self.collab_follow(None);
            return self.echo(format!("Stopped following {name}."));
        }
        let Some(lead) = lead else { return };
        // Go to the layout they are on (when the drawing has it).
        if lead.layout != layout_name(&st.space) {
            let to = if lead.layout.is_empty() { Space::Model } else { Space::Paper(lead.layout.clone()) };
            if to != Space::Model && !st.doc.layouts.iter().any(|l| l.name == lead.layout) {
                return;
            }
            st.space = to;
            st.mspace = None;
            st.revision += 1;
        }
        // A follower inside a layout viewport goes back to the sheet, whose view is shared.
        if st.mspace.is_some() {
            st.mspace = None;
            st.revision += 1;
        }
        let Some([x, y, h]) = lead.view else { return };
        let v = View { center: Vec2::new(x, y), height: h };
        if !same_view(&v, &st.view()) {
            st.set_view_quiet(v);
        }
        let now = st.view();
        if let Some(l) = self.collab.as_mut() {
            l.followed = Some(now);
        }
    }

    /// Lock the block open in the shared drawing's Block Editor, or let go of it once closed. A
    /// block someone else holds closes the editor again, unchanged.
    fn collab_sync_lock(&mut self) {
        let Some(live) = self.collab.as_ref().filter(|l| l.client.is_some() && l.ended.is_none()) else { return };
        let uid = live.uid;
        let open = self
            .docs
            .iter()
            .find(|d| d.uid == uid)
            .and_then(|d| d.block_editor().map(str::to_string).or_else(|| d.doc.refedit.as_ref().map(|r| r.block.clone())));
        // Two people opened the same block at the same moment: the server gave it to the other.
        if let (Some(mine), Some(c)) = (&live.locked, live.client.as_ref())
            && let Some(h) = c.lock_holder(mine).filter(|h| *h != c.id())
        {
            let who = c.peers().iter().find(|p| p.client == h).map_or_else(|| "someone else".to_string(), |p| p.name.clone());
            let msg = format!("{who} opened block {mine} a moment before you; it is theirs until they close it.");
            if let Some(l) = self.collab.as_mut() {
                l.locked = None;
            }
            if let Some(st) = self.docs.iter_mut().find(|d| d.uid == uid)
                && st.block_edit.is_some()
            {
                let _ = st.close_block_editor(false);
            }
            if self.collab_editing_in_place() {
                let _ = self.execute("refclose", &serde_json::json!({"save": false}));
            }
            return self.echo(msg);
        }
        if open.as_deref().map(str::to_lowercase) == live.locked.as_deref().map(str::to_lowercase) {
            return;
        }
        let holder = |name: &str| {
            live.client.as_ref().and_then(|c| {
                c.lock_holder(name).filter(|h| *h != c.id()).map(|h| (h, c.peers().iter().find(|p| p.client == h).map(|p| p.name.clone())))
            })
        };
        if let Some(name) = &open
            && let Some((_, who)) = holder(name)
        {
            let in_place = self.docs.iter().find(|d| d.uid == uid).is_some_and(|d| d.block_edit.is_none());
            if in_place {
                let _ = self.execute("refclose", &serde_json::json!({"save": false}));
            } else if let Some(st) = self.docs.iter_mut().find(|d| d.uid == uid) {
                let _ = st.close_block_editor(false);
            }
            let who = who.unwrap_or_else(|| "someone else".into());
            return self.echo(format!("Block {name} is being edited by {who}; try again when they close it."));
        }
        let Some(live) = self.collab.as_mut() else { return };
        let mut msgs = Vec::new();
        if let Some(old) = live.locked.take() {
            msgs.push(ClientMsg::Lock { block: old, on: false });
        }
        if let Some(new) = open {
            msgs.push(ClientMsg::Lock { block: new.clone(), on: true });
            live.locked = Some(new);
        }
        for m in msgs {
            if let Err(e) = live.link.send(&m) {
                return self.collab_end(format!("the connection to the server failed: {e}"));
            }
        }
    }

    fn collab_end(&mut self, why: String) {
        self.echo(format!("Shared session ended: {why}"));
        if let Some(l) = self.collab.as_mut() {
            l.ended = Some(why);
        }
    }

    fn collab_receive(&mut self, msg: ServerMsg) -> std::result::Result<(), String> {
        let Some(live) = self.collab.as_mut() else { return Ok(()) };
        let uid = live.uid;
        let Some(st) = self.docs.iter_mut().find(|d| d.uid == uid) else {
            return Err("the shared drawing was closed".into());
        };
        let mut rejoined = None;
        let view = match (&mut live.client, msg) {
            (None, ServerMsg::Welcome { client, seq, snapshot, file, title }) => {
                let (c, view) = Client::welcome(client, seq, &snapshot).map_err(|e| e.to_string())?;
                live.client = Some(c);
                // A new file's address is known once the server named it.
                if let Some(server) = live.url.strip_suffix("/new").map(str::to_string)
                    && !file.is_empty()
                {
                    live.url = format!("{server}/s/{file}");
                }
                if !title.is_empty() {
                    st.title = title.clone();
                }
                live.title = title;
                // Snapshot undo can't work across other people's edits (per-user undo is P4).
                st.undo.clear();
                st.redo.clear();
                Some(view)
            }
            (None, ServerMsg::Error { message }) => return Err(message),
            (None, _) => None,
            (Some(_), ServerMsg::Welcome { client, seq, snapshot, .. }) if live.rejoining => {
                live.rejoining = false;
                // The server let go of this person's block lock and presence when they dropped.
                live.locked = None;
                live.sent = None;
                let Some(old) = live.client.take() else { return Ok(()) };
                let was = old.id();
                let (c, view, msg) = old.rejoin(client, seq, &snapshot, st.drawing()).map_err(|e| e.to_string())?;
                live.client = Some(c);
                if let Some(m) = msg {
                    live.link.send(&m).map_err(|e| e.to_string())?;
                }
                rejoined = Some(if client == was {
                    "Reconnected.".to_string()
                } else {
                    "Reconnected under a new client id: objects you drew while away may be refused.".to_string()
                });
                Some(view)
            }
            (Some(c), msg) => c.receive(msg, st.drawing()).map_err(|e| e.to_string())?.view,
        };
        let notices = live.client.as_mut().map(Client::take_notices).unwrap_or_default();
        if let Some(view) = view {
            // With the Block Editor open, others' edits go to the drawing behind it.
            let view = Arc::new(view);
            *st.drawing_arc_mut() = view.clone();
            if st.block_edit.is_none() {
                st.selection.retain(|h| view.entity(*h).is_some());
            }
            st.revision += 1;
            live.known = Some(view);
        }
        notices.into_iter().chain(rejoined).for_each(|n| self.echo(n));
        Ok(())
    }

    /// Send what a command changed in drawing `uid` (it started from `before`).
    pub(crate) fn collab_after_edit(&mut self, before: &Arc<Drawing>, uid: u64, id: &str) {
        if id.starts_with("collab.") {
            return;
        }
        self.collab_sync_lock();
        let Some(after) = self.docs.iter().find(|d| d.uid == uid).map(|d| d.drawing().clone()) else { return };
        let Some(live) = self.collab.as_mut().filter(|l| l.uid == uid && l.ended.is_none()) else { return };
        // Edits made while the connection is down go out when it is back (`Client::rejoin`).
        if live.retry.is_some() || live.rejoining {
            return;
        }
        // Inside REFEDIT nothing goes out until REFCLOSE, which sends the finished block.
        if self.docs.iter().find(|d| d.uid == uid).is_some_and(|d| d.doc.refedit.is_some()) {
            return;
        }
        let Some(live) = self.collab.as_mut().filter(|l| l.uid == uid && l.ended.is_none()) else { return };
        let Some(client) = live.client.as_mut() else { return };
        let before = live.known.clone().unwrap_or_else(|| before.clone());
        if Arc::ptr_eq(&before, &after) {
            return;
        }
        live.known = Some(after.clone());
        let sent = match client.local(&before, &after) {
            Ok(Some(msg)) => live.link.send(&msg),
            Ok(None) => Ok(()),
            Err(e) => Err(e),
        };
        if let Err(e) = sent {
            // A link that broke while sending: try again like a dropped one; the edit goes out then.
            match self.collab.as_mut().filter(|l| !l.url.is_empty()) {
                Some(live) => {
                    let now = Instant::now();
                    live.retry = Some(Retry { since: now, next: now + Duration::from_secs(1), tries: 0, connecting: None });
                    self.echo("Connection to the server lost; reconnecting…");
                }
                None => self.collab_end(format!("{id}'s changes could not be sent: {e}")),
            }
        }
    }
}

/// The presence name of a space: "" for model space, else the layout's name.
fn layout_name(space: &Space) -> String {
    match space {
        Space::Model => String::new(),
        Space::Paper(name) => name.clone(),
    }
}

fn same_view(a: &View, b: &View) -> bool {
    let tol = 1e-9 * a.height.abs().max(b.height.abs()).max(1.0);
    (a.center.x - b.center.x).abs() <= tol && (a.center.y - b.center.y).abs() <= tol && (a.height - b.height).abs() <= tol
}

impl crate::DocState {
    /// Take back undo or redo step `snap` in a shared drawing: reverse what its parts changed,
    /// newest first, leaving what someone changed since. Returns the step that brings it back and
    /// how many changes were left. A step from before the session (it has no parts) can't be
    /// taken back this way.
    pub(crate) fn revert_in(&mut self, snap: &crate::Snapshot) -> std::result::Result<(crate::Snapshot, usize), String> {
        if snap.parts.is_empty() {
            return Err("this step was made before the drawing was shared".into());
        }
        let now = self.doc.clone();
        let mut d = (*now).clone();
        let mut left = 0;
        for (before, after) in snap.parts.iter().rev() {
            let (next, n) = cadcraft_collab::undo::revert(&d, before, after).map_err(|e| e.to_string())?;
            d = next;
            left += n;
        }
        let new = Arc::new(d);
        self.doc = new.clone();
        let selection = std::mem::take(&mut self.selection);
        self.follow_layout(&now);
        self.revision += 1;
        let mut back = crate::Snapshot::new(snap.label.clone(), now.clone(), selection);
        back.parts.push((now, new));
        Ok((back, left))
    }
}

/// Objects that edits since `before` left pointing at a table entry that doesn't exist (someone
/// renamed or deleted it while another used it): a layer, linetype, text style or dimension style
/// that was renamed is followed to its new name. Otherwise a missing layer is made again with
/// default settings, a linetype goes back to ByLayer and a style to Standard (when the drawing
/// has it). `renamed(table, name)` gives a rename seen in the session. Returns how many objects
/// or layers changed.
fn repair_references(d: &mut Arc<Drawing>, before: &Drawing, renamed: impl Fn(&str, &str) -> Option<String>) -> usize {
    use cadcraft_doc::EntityKind;
    enum Fix {
        Layer(String),
        Linetype(String),
        Style(String),
    }
    let follow = |table: &str, name: &str, exists: &dyn Fn(&str) -> bool| renamed(table, name).filter(|n| exists(n));
    let mut fixes: Vec<(Handle, Fix)> = Vec::new();
    let mut make: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    {
        let dr: &Drawing = d;
        let layer_ok = |n: &str| dr.layer(n).is_some();
        let linetype_ok =
            |n: &str| n.is_empty() || ["bylayer", "byblock", "continuous"].contains(&n.to_ascii_lowercase().as_str()) || dr.linetype(n).is_some();
        let text_ok = |n: &str| dr.text_style(n).is_some();
        let dim_ok = |n: &str| dr.dim_style(n).is_some();
        for store in std::iter::once(&dr.model).chain(dr.layouts.iter().map(|l| &l.entities)) {
            for e in store.iter() {
                if before.entity(e.handle).is_some_and(|x| Arc::ptr_eq(x, e)) {
                    continue;
                }
                let c = &e.common;
                if !layer_ok(&c.layer) {
                    match follow("layers", &c.layer, &layer_ok) {
                        Some(to) => fixes.push((e.handle, Fix::Layer(to))),
                        None => {
                            make.insert(c.layer.clone());
                        }
                    }
                }
                if !linetype_ok(&c.linetype) {
                    fixes.push((e.handle, Fix::Linetype(follow("linetypes", &c.linetype, &linetype_ok).unwrap_or_else(|| "ByLayer".into()))));
                }
                let (style, table, ok): (Option<&String>, &str, &dyn Fn(&str) -> bool) = match &e.kind {
                    EntityKind::Text(t) => (Some(&t.style), "text_styles", &text_ok),
                    EntityKind::MText(t) => (Some(&t.style), "text_styles", &text_ok),
                    EntityKind::Dimension(dm) => (Some(&dm.style), "dim_styles", &dim_ok),
                    _ => (None, "", &text_ok),
                };
                if let Some(style) = style.filter(|s| !ok(s))
                    && let Some(to) = follow(table, style, ok).or_else(|| ok("Standard").then(|| "Standard".to_string()))
                {
                    fixes.push((e.handle, Fix::Style(to)));
                }
            }
        }
    }
    let n = fixes.len() + make.len();
    if n > 0 {
        let d = Arc::make_mut(d);
        for (h, fix) in fixes {
            let _ = d.modify_entity(h, |e| match fix {
                Fix::Layer(to) => e.common.layer = to,
                Fix::Linetype(to) => e.common.linetype = to,
                Fix::Style(to) => match &mut e.kind {
                    EntityKind::Text(t) => t.style = to,
                    EntityKind::MText(t) => t.style = to,
                    EntityKind::Dimension(dm) => dm.style = to,
                    _ => {}
                },
            });
        }
        d.layers.extend(make.iter().map(|name| cadcraft_doc::Layer::new(name)));
    }
    n
}
