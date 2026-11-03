//! Shared sessions (the fork's multiplayer work, `plan/multiplayer-design.md`): one open drawing
//! is edited by several people through a session server. After each command that changed it, the
//! drawing's changes go to the server ([`Session::collab_after_edit`]); between commands,
//! [`Session::collab_tick`] applies what the server sent. Nothing arrives while a command runs, so
//! a command never sees the drawing change under it.
//!
//! Undo in a session takes back only this person's steps ([`DocState::revert_in`]): swapping the
//! drawing back would take back other people's edits too. The Block Editor and REFEDIT, which
//! swap the whole drawing, are refused (P5).

use std::sync::Arc;

pub use cadcraft_collab::Link;
use cadcraft_collab::client::Client;
use cadcraft_collab::message::{ClientMsg, MAX_SELECTION, Peer, Presence, ServerMsg};
use cadcraft_collab::snapshot;
use cadcraft_doc::{Drawing, Space};
use cadcraft_geom::Vec2;

use crate::view::View;
use crate::{EngineError, Result, Session};

/// Most messages applied in one tick, so a burst doesn't stall a frame.
const MAX_PER_TICK: usize = 256;

/// Least time between two presence messages, in seconds (20 a second at most).
const PRESENCE_EVERY: f64 = 0.05;

/// Commands that can't run in a shared session yet.
const REFUSED: &[&str] = &["bedit", "refedit"];

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
}

impl Session {
    /// Join a session over `link` with the active drawing. `host`: start the session with this
    /// drawing (ignored if the session already has one). The welcome arrives through
    /// [`Session::collab_tick`].
    pub fn collab_attach(&mut self, link: Box<dyn Link>, name: &str, host: bool) -> Result<()> {
        self.collab_attach_to(link, "", name, host, false)
    }

    /// [`Session::collab_attach`] to the session at `url`; `create`: it must be a new file.
    pub fn collab_attach_to(&mut self, link: Box<dyn Link>, url: &str, name: &str, host: bool, create: bool) -> Result<()> {
        if self.collab.as_ref().is_some_and(|l| l.ended.is_none()) {
            return Err(EngineError::Other("already in a shared session (collab.leave first)".into()));
        }
        let st = self.state()?;
        let seed = if host { Some(snapshot::encode(&st.doc).map_err(|e| EngineError::Other(e.to_string()))?) } else { None };
        let uid = st.uid;
        let mut link = link;
        link.send(&ClientMsg::Hello { name: name.to_string(), seed, create }).map_err(|e| EngineError::Other(e.to_string()))?;
        self.collab =
            Some(Live { link, uid, client: None, ended: None, url: url.to_string(), sent: None, sent_at: 0.0, following: None, followed: None });
        Ok(())
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
        self.collab_drain();
        self.collab_follow_step();
    }

    fn collab_drain(&mut self) {
        for _ in 0..MAX_PER_TICK {
            let Some(live) = self.collab.as_mut().filter(|l| l.ended.is_none()) else { return };
            let msg = match live.link.recv() {
                Some(Ok(m)) => m,
                Some(Err(e)) => return self.collab_end(format!("a message from the server could not be read: {e}")),
                None if !live.link.is_open() => return self.collab_end("the connection to the server closed".into()),
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
        let Some(live) = self.collab.as_ref().filter(|l| l.client.is_some() && l.ended.is_none()) else { return };
        let uid = live.uid;
        let mut p = Presence::default();
        if let Some(st) = self.docs.get(self.active).filter(|d| d.uid == uid) {
            p.layout = layout_name(&st.space);
            p.cursor = cursor.filter(|c| c.is_finite()).map(|c| [c.x, c.y]);
            p.selection = st.selection.iter().take(MAX_SELECTION).copied().collect();
            p.command = self.running.as_ref().map(|r| r.id.to_ascii_uppercase());
            let v = st.view();
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
        let Some(lead) = lead.filter(|p| p.layout == layout_name(&st.space)) else { return };
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
        let view = match (&mut live.client, msg) {
            (None, ServerMsg::Welcome { client, seq, snapshot }) => {
                let (c, view) = Client::welcome(client, seq, &snapshot).map_err(|e| e.to_string())?;
                live.client = Some(c);
                // Snapshot undo can't work across other people's edits (per-user undo is P4).
                st.undo.clear();
                st.redo.clear();
                Some(view)
            }
            (None, ServerMsg::Error { message }) => return Err(message),
            (None, _) => None,
            (Some(c), msg) => c.receive(msg, &st.doc).map_err(|e| e.to_string())?.view,
        };
        let notices = live.client.as_mut().map(Client::take_notices).unwrap_or_default();
        if let Some(view) = view {
            st.doc = Arc::new(view);
            let doc = st.doc.clone();
            st.selection.retain(|h| doc.entity(*h).is_some());
            st.revision += 1;
        }
        notices.into_iter().for_each(|n| self.echo(n));
        Ok(())
    }

    /// Send what a command changed in drawing `uid` (it started from `before`).
    pub(crate) fn collab_after_edit(&mut self, before: &Arc<Drawing>, uid: u64, id: &str) {
        if id.starts_with("collab.") {
            return;
        }
        let Some(live) = self.collab.as_mut().filter(|l| l.uid == uid && l.ended.is_none()) else { return };
        let Some(client) = live.client.as_mut() else { return };
        let Some(after) = self.docs.iter().find(|d| d.uid == uid).map(|d| d.doc.clone()) else { return };
        if Arc::ptr_eq(before, &after) {
            return;
        }
        let sent = match client.local(before, &after) {
            Ok(Some(msg)) => live.link.send(&msg),
            Ok(None) => Ok(()),
            Err(e) => Err(e),
        };
        if let Err(e) = sent {
            self.collab_end(format!("{id}'s changes could not be sent: {e}"));
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
