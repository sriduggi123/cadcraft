//! The client's state machine. It keeps the drawing as the server confirmed it and the batches it
//! sent that the server hasn't answered yet. The person's own edits show at once; when someone
//! else's batch arrives, the view is rebuilt as the confirmed drawing with the pending batches
//! replayed on top, so a pending edit never flickers away.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use cadcraft_doc::{Drawing, Handle};

use crate::message::{ClientMsg, Peer, Presence, ServerMsg};
use crate::{Change, Error, Result, apply, diff, snapshot, space, vars};

/// Most renames remembered, and the longest chain of them followed.
const MAX_RENAMES: usize = 10_000;
const MAX_RENAME_CHAIN: usize = 64;
/// This person's own accepted batches remembered for conflict warnings.
const MAX_MINE: usize = 64;
/// Accepted batches remembered to find the ones made at the same time.
const MAX_RECENT: usize = 256;

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
    /// Table entries renamed in this session: (field, old name) → new name.
    renames: BTreeMap<(String, String), String>,
    /// The last accepted batches: (seq, author, objects touched).
    recent: VecDeque<(u64, u32, BTreeSet<Handle>)>,
    /// What this person's last accepted batches wrote: (seq, keys).
    mine: VecDeque<(u64, BTreeSet<crate::undo::Key>)>,
    /// Blocks locked for the Block Editor (lower case) and who holds each.
    locks: BTreeMap<String, u32>,
    /// See [`Client::take_concurrent`].
    concurrent: Vec<(BTreeSet<Handle>, BTreeSet<Handle>)>,
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
            renames: BTreeMap::new(),
            recent: VecDeque::new(),
            mine: VecDeque::new(),
            locks: BTreeMap::new(),
            concurrent: Vec::new(),
            notices: Vec::new(),
        };
        Ok((c, view))
    }

    /// Back after a dropped connection: the server's new welcome, with this person's edits it may
    /// not have (unconfirmed or made while away) replayed on top. Returns the client, the drawing
    /// to show and the batch that sends those edits (none when the server had them all).
    pub fn rejoin(self, client: u32, seq: u64, bytes: &[u8], view: &Drawing) -> Result<(Client, Drawing, Option<ClientMsg>)> {
        let mine = diff(&self.confirmed, view)?;
        let same = client == self.id;
        let (mut c, base) = Client::welcome(client, seq, bytes)?;
        c.renames = self.renames;
        let mut d = base.clone();
        crate::apply::apply_lenient(&mut d, &mine);
        carry_local(view, &mut d);
        space::enter(&mut d, client);
        // Handles made and deleted before the drop are never handed out again.
        if same {
            d.handseed = d.handseed.max(view.handseed);
        }
        let msg = c.local(&base, &d)?;
        Ok((c, d, msg))
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

    /// What table entry `name` of `field` is called now, after the renames seen in this session.
    pub fn renamed(&self, field: &str, name: &str) -> Option<&str> {
        let mut at = self.renames.get(&(field.to_string(), name.to_string()))?;
        for _ in 0..MAX_RENAME_CHAIN {
            match self.renames.get(&(field.to_string(), at.clone())) {
                Some(next) if next != name => at = next,
                _ => break,
            }
        }
        Some(at.as_str())
    }

    /// Pairs of objects that batches made at the same time touched (one batch's, the other's),
    /// each once: where a dimension and its object, or a hatch and its boundary, were edited by
    /// two people at once and may no longer agree.
    pub fn take_concurrent(&mut self) -> Vec<(BTreeSet<Handle>, BTreeSet<Handle>)> {
        std::mem::take(&mut self.concurrent)
    }

    fn note_concurrent(&mut self, seq: u64, client: u32, base: u64, changes: &[Change]) {
        let touched: BTreeSet<Handle> = changes
            .iter()
            .filter_map(|c| match c {
                Change::Delete { id } | Change::Create { id, .. } | Change::Common { id, .. } | Change::Geometry { id, .. } => Some(*id),
                _ => None,
            })
            .collect();
        for (s, who, other) in &self.recent {
            if *s > base && *who != client && self.concurrent.len() < MAX_RECENT {
                self.concurrent.push((touched.clone(), other.clone()));
            }
        }
        self.recent.push_back((seq, client, touched));
        if self.recent.len() > MAX_RECENT {
            self.recent.pop_front();
        }
    }

    /// Tell this person when someone, editing at the same time, changed what they had just
    /// changed: the later batch is the one kept.
    fn note_overwrites(&mut self, seq: u64, client: u32, base: u64, changes: &[Change]) {
        let keys: BTreeSet<crate::undo::Key> = changes.iter().map(crate::undo::key).collect();
        if client == self.id {
            self.mine.push_back((seq, keys));
            if self.mine.len() > MAX_MINE {
                self.mine.pop_front();
            }
            return;
        }
        let hit: BTreeSet<&crate::undo::Key> = self.mine.iter().filter(|(s, _)| *s > base).flat_map(|(_, k)| k.intersection(&keys)).collect();
        if hit.is_empty() {
            return;
        }
        let who = self.peers.iter().find(|p| p.client == client).map_or("Someone", |p| p.name.as_str());
        let what = if hit.len() == 1 { "1 thing".to_string() } else { format!("{} things", hit.len()) };
        self.notices.push(format!("{who} changed {what} you had just changed, at the same time; their change is the one kept."));
    }

    fn note_renames(&mut self, changes: &[Change]) {
        for c in changes {
            if let Change::Entry { field, name, value: Some(v) } = c
                && let Some(new) = v.0.get("name").and_then(serde_json::Value::as_str)
                && new != name
                && self.renames.len() < MAX_RENAMES
            {
                self.renames.insert((field.clone(), name.clone()), new.to_string());
            }
        }
    }

    /// Who holds the lock on block `name`, if anyone.
    pub fn lock_holder(&self, name: &str) -> Option<u32> {
        self.locks.get(&name.to_lowercase()).copied()
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
        Ok(Some(ClientMsg::Batch { batch, base: self.seq, changes }))
    }

    /// A message from the server. `view` is the drawing shown now.
    pub fn receive(&mut self, msg: ServerMsg, view: &Drawing) -> Result<Received> {
        match msg {
            ServerMsg::Accepted { seq, client, batch, base, changes } => {
                apply(&mut self.confirmed, &changes)?;
                self.note_renames(&changes);
                self.note_concurrent(seq, client, base, &changes);
                self.note_overwrites(seq, client, base, &changes);
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
            ServerMsg::Locks { locks } => {
                self.locks = locks.into_iter().collect();
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
            // Only the status page and `/ai` send them; a session never does.
            ServerMsg::Files { .. } | ServerMsg::Ai { .. } => Ok(Received::default()),
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
/// session-only data (hidden objects, REFEDIT, read notes, loaded image pixels).
pub fn carry_local(from: &Drawing, to: &mut Drawing) {
    to.handseed = from.handseed;
    to.header.vars.retain(|k, _| !vars::is_local(k));
    to.header.vars.extend(from.header.vars.iter().filter(|(k, _)| vars::is_local(k)).map(|(k, v)| (k.clone(), v.clone())));
    to.hidden_objects = from.hidden_objects.clone();
    to.read_notes = from.read_notes.clone();
    to.refedit = from.refedit.clone();
    to.skipped_records = from.skipped_records.clone();
    to.portable_fonts = from.portable_fonts;
    to.rasters = from.rasters.clone();
}
