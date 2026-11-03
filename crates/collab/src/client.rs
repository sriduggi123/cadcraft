//! The client's state machine. It keeps the drawing as the server confirmed it and the batches it
//! sent that the server hasn't answered yet. The person's own edits show at once; when someone
//! else's batch arrives, the view is rebuilt as the confirmed drawing with the pending batches
//! replayed on top, so a pending edit never flickers away.

use std::collections::{BTreeMap, VecDeque};

use cadcraft_doc::Drawing;

use crate::message::{ClientMsg, Peer, Presence, ServerMsg};
use crate::{Change, Error, Result, apply, diff, snapshot, space, vars};

#[derive(Debug)]
pub struct Client {
    id: u32,
    seq: u64,
    confirmed: Drawing,
    pending: VecDeque<(u64, Vec<Change>)>,
    next_batch: u64,
    peers: Vec<Peer>,
    /// The others' presence, by client.
    others: BTreeMap<u32, Presence>,
    notices: Vec<String>,
}

/// What a message did to the client.
#[derive(Debug, Default)]
pub struct Received {
    /// The drawing to show now, when it changed.
    pub view: Option<Drawing>,
}

impl Client {
    /// The client a `Welcome` makes, and the drawing to show: the session's drawing, allocating
    /// handles in the client's own space.
    pub fn welcome(client: u32, seq: u64, bytes: &[u8]) -> Result<(Client, Drawing)> {
        let confirmed = snapshot::decode(bytes)?;
        let mut view = confirmed.clone();
        space::enter(&mut view, client);
        let c = Client {
            id: client,
            seq,
            confirmed,
            pending: VecDeque::new(),
            next_batch: 1,
            peers: Vec::new(),
            others: BTreeMap::new(),
            notices: Vec::new(),
        };
        Ok((c, view))
    }

    pub fn id(&self) -> u32 {
        self.id
    }

    /// The last batch number the server confirmed to this client.
    pub fn seq(&self) -> u64 {
        self.seq
    }

    pub fn peers(&self) -> &[Peer] {
        &self.peers
    }

    /// Where the others are and what they're doing, by client.
    pub fn others(&self) -> &BTreeMap<u32, Presence> {
        &self.others
    }

    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    /// Messages for the person (a refused edit), each once.
    pub fn take_notices(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notices)
    }

    /// A local edit: the batch to send, if the edit changed anything that syncs.
    pub fn local(&mut self, before: &Drawing, after: &Drawing) -> Result<Option<ClientMsg>> {
        let changes = diff(before, after)?;
        if changes.is_empty() {
            return Ok(None);
        }
        let batch = self.next_batch;
        self.next_batch += 1;
        self.pending.push_back((batch, changes.clone()));
        Ok(Some(ClientMsg::Batch { batch, changes }))
    }

    /// A message from the server. `view` is the drawing shown now.
    pub fn receive(&mut self, msg: ServerMsg, view: &Drawing) -> Result<Received> {
        match msg {
            ServerMsg::Accepted { seq, client, batch, changes } => {
                apply(&mut self.confirmed, &changes)?;
                self.seq = seq;
                if client == self.id {
                    match self.pending.pop_front() {
                        Some((b, _)) if b == batch => return Ok(Received::default()),
                        _ => return Err(Error::Invalid(format!("the server confirmed batch {batch} out of order"))),
                    }
                }
                Ok(Received { view: Some(self.rebuild(view)) })
            }
            ServerMsg::Rejected { batch, reason } => {
                self.pending.retain(|(b, _)| *b != batch);
                self.notices.push(format!("Your last change was not kept: {reason}"));
                Ok(Received { view: Some(self.rebuild(view)) })
            }
            ServerMsg::Peers { peers } => {
                self.others.retain(|c, _| peers.iter().any(|p| p.client == *c));
                self.peers = peers;
                Ok(Received::default())
            }
            ServerMsg::Presence { client, presence } => {
                if client != self.id && self.peers.iter().any(|p| p.client == client) {
                    self.others.insert(client, presence);
                }
                Ok(Received::default())
            }
            ServerMsg::Error { message } => Err(Error::Invalid(message)),
            ServerMsg::Welcome { .. } => Err(Error::Invalid("a second welcome".into())),
            // Only the file list sends it; a session never does.
            ServerMsg::Files { .. } => Ok(Received::default()),
        }
    }

    /// The confirmed drawing with the pending batches replayed (a change that no longer applies,
    /// e.g. to an object someone deleted, is skipped: the server will refuse that batch).
    fn rebuild(&self, view: &Drawing) -> Drawing {
        let mut d = self.confirmed.clone();
        for (_, changes) in &self.pending {
            crate::apply::apply_lenient(&mut d, changes);
        }
        carry_local(view, &mut d);
        d
    }
}

/// Copy what never syncs from `from` to `to`: per-person variables, the handle seed and
/// session-only data (hidden objects, REFEDIT, read notes).
pub fn carry_local(from: &Drawing, to: &mut Drawing) {
    to.handseed = from.handseed;
    to.header.vars.retain(|k, _| !vars::is_local(k));
    to.header.vars.extend(from.header.vars.iter().filter(|(k, _)| vars::is_local(k)).map(|(k, v)| (k.clone(), v.clone())));
    to.hidden_objects = from.hidden_objects.clone();
    to.read_notes = from.read_notes.clone();
    to.refedit = from.refedit.clone();
    to.skipped_records = from.skipped_records.clone();
    to.portable_fonts = from.portable_fonts;
}
