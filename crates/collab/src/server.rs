//! The session server's state machine: it holds the authoritative drawing, numbers every
//! accepted batch and sends it to everyone, and refuses a batch that doesn't apply as a whole
//! (decision D2). No networking: the transport feeds it messages and delivers what it returns.

use std::collections::{BTreeMap, BTreeSet};

use cadcraft_doc::{Drawing, Handle};

use crate::message::{MAX_SELECTION, Peer, Presence, ServerMsg};
use crate::{Change, Error, Result, apply, snapshot, space};

/// Most changes in one batch.
pub const MAX_CHANGES: usize = 200_000;

/// Who a reply goes to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum To {
    One(u32),
    All,
    /// Everyone but this client.
    Others(u32),
}

impl To {
    /// Whether a message for `self` goes to `client`.
    pub fn reaches(self, client: u32) -> bool {
        match self {
            To::One(c) => c == client,
            To::All => true,
            To::Others(c) => c != client,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Outgoing {
    pub to: To,
    pub msg: ServerMsg,
}

#[derive(Debug, Default)]
pub struct Server {
    /// The drawing; `None` until the first person to join brings one.
    drawing: Option<Drawing>,
    seq: u64,
    next_client: u32,
    clients: BTreeMap<u32, String>,
    /// Handles deleted this session: creating one again is allowed to anyone (undo, moves).
    deleted: BTreeSet<Handle>,
    /// Each person's last presence, for people who join later.
    presence: BTreeMap<u32, Presence>,
}

impl Server {
    /// A session over `drawing`, or one that takes its drawing from the first `Hello` with a seed.
    pub fn new(drawing: Option<Drawing>) -> Server {
        let mut s = Server { drawing: None, ..Server::default() };
        if let Some(d) = drawing {
            s.start(d);
        }
        s
    }

    fn start(&mut self, d: Drawing) {
        // Client spaces start above every space the drawing already uses, so a new client never
        // continues someone else's handles from an earlier session.
        let used = crate::container::stores(&d).into_iter().flat_map(|(_, s)| s.iter().map(|e| space::space_of(e.handle))).max();
        self.next_client = self.next_client.max(used.unwrap_or(0).saturating_add(1)).max(1);
        self.drawing = Some(d);
    }

    pub fn drawing(&self) -> Option<&Drawing> {
        self.drawing.as_ref()
    }

    pub fn seq(&self) -> u64 {
        self.seq
    }

    pub fn peers(&self) -> Vec<Peer> {
        self.clients.iter().map(|(c, n)| Peer { client: *c, name: n.clone() }).collect()
    }

    /// Someone joins: their id, and the messages to send (their welcome, everyone's peer list).
    pub fn join(&mut self, name: &str, seed: Option<&[u8]>) -> Result<(u32, Vec<Outgoing>)> {
        if self.drawing.is_none() {
            let d = match seed {
                Some(bytes) => snapshot::decode(bytes)?,
                None => Drawing::default(),
            };
            self.start(d);
        }
        let d = self.drawing.as_ref().ok_or_else(|| Error::Invalid("no drawing".into()))?;
        let client = self.next_client;
        self.next_client = self.next_client.checked_add(1).ok_or_else(|| Error::Invalid("no client ids left".into()))?;
        let welcome = ServerMsg::Welcome { client, seq: self.seq, snapshot: snapshot::encode(d)? };
        self.clients.insert(client, name.chars().take(64).collect());
        let mut out = vec![Outgoing { to: To::One(client), msg: welcome }, self.peers_msg()];
        out.extend(self.presence.iter().map(|(c, p)| Outgoing { to: To::One(client), msg: ServerMsg::Presence { client: *c, presence: p.clone() } }));
        Ok((client, out))
    }

    /// Someone's presence: kept, and passed on to everyone else.
    pub fn presence(&mut self, client: u32, mut presence: Presence) -> Vec<Outgoing> {
        if !self.clients.contains_key(&client) {
            return Vec::new();
        }
        presence.selection.truncate(MAX_SELECTION);
        presence.layout = presence.layout.chars().take(256).collect();
        presence.command = presence.command.map(|c| c.chars().take(64).collect());
        self.presence.insert(client, presence.clone());
        vec![Outgoing { to: To::Others(client), msg: ServerMsg::Presence { client, presence } }]
    }

    /// Someone leaves (or their link dropped).
    pub fn leave(&mut self, client: u32) -> Vec<Outgoing> {
        self.presence.remove(&client);
        if self.clients.remove(&client).is_some() { vec![self.peers_msg()] } else { Vec::new() }
    }

    fn peers_msg(&self) -> Outgoing {
        Outgoing { to: To::All, msg: ServerMsg::Peers { peers: self.peers() } }
    }

    /// A batch from `client`: accepted and sent to everyone, or refused to its author.
    pub fn batch(&mut self, client: u32, batch: u64, changes: Vec<Change>) -> Vec<Outgoing> {
        match self.check(client, &changes) {
            Ok(d) => {
                self.drawing = Some(d);
                self.deleted.extend(changes.iter().filter_map(|c| match c {
                    Change::Delete { id } => Some(*id),
                    _ => None,
                }));
                self.seq += 1;
                vec![Outgoing { to: To::All, msg: ServerMsg::Accepted { seq: self.seq, client, batch, changes } }]
            }
            Err(e) => vec![Outgoing { to: To::One(client), msg: ServerMsg::Rejected { batch, reason: e.to_string() } }],
        }
    }

    /// The drawing with the batch applied, if it is valid as a whole.
    fn check(&self, client: u32, changes: &[Change]) -> Result<Drawing> {
        if !self.clients.contains_key(&client) {
            return Err(Error::Invalid("not in the session".into()));
        }
        if changes.len() > MAX_CHANGES {
            return Err(Error::Invalid(format!("more than {MAX_CHANGES} changes in one batch")));
        }
        let mut deleted_here = BTreeSet::new();
        for c in changes {
            match c {
                Change::Delete { id } => {
                    deleted_here.insert(*id);
                }
                Change::Create { id, .. } if space::space_of(*id) != client && !self.deleted.contains(id) && !deleted_here.contains(id) => {
                    return Err(Error::Invalid(format!("{id:?} is not in your handle space")));
                }
                _ => {}
            }
        }
        let mut d = self.drawing.clone().ok_or_else(|| Error::Invalid("no drawing".into()))?;
        apply(&mut d, changes)?;
        Ok(d)
    }
}
