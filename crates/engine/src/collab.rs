//! Shared sessions (the fork's multiplayer work, `plan/multiplayer-design.md`): one open drawing
//! is edited by several people through a session server. After each command that changed it, the
//! drawing's changes go to the server ([`Session::collab_after_edit`]); between commands,
//! [`Session::collab_tick`] applies what the server sent. Nothing arrives while a command runs, so
//! a command never sees the drawing change under it.
//!
//! Snapshot undo would take back other people's edits, so U, UNDO and REDO are refused in a
//! session until per-user undo lands (P4), and so are the Block Editor and REFEDIT, which swap the
//! whole drawing (P5).

use std::sync::Arc;

use cadcraft_collab::client::Client;
use cadcraft_collab::message::{ClientMsg, Peer, ServerMsg};
use cadcraft_collab::{Link, snapshot};
use cadcraft_doc::Drawing;

use crate::{EngineError, Result, Session};

/// Most messages applied in one tick, so a burst doesn't stall a frame.
const MAX_PER_TICK: usize = 256;

/// Commands that can't run in a shared session yet.
const REFUSED: &[&str] = &["bedit", "refedit", "undo", "u", "redo", "mredo", "oops"];

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
        self.collab = Some(Live { link, uid, client: None, ended: None, url: url.to_string() });
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

    /// Whether `id` is refused while the active drawing is shared.
    pub(crate) fn collab_refuses(&self, id: &str) -> Option<String> {
        let shared = self.collab_live() && self.state().is_ok_and(|st| self.collab.as_ref().is_some_and(|l| l.uid == st.uid));
        (shared && REFUSED.contains(&id)).then(|| format!("{} isn't available in a shared session yet", id.to_ascii_uppercase()))
    }

    /// Apply what the server sent. Call between commands (the UI calls it every frame).
    pub fn collab_tick(&mut self) {
        if self.running.is_some() || self.lisp_busy() {
            return;
        }
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
