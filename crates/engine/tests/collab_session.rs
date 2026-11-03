//! Shared sessions converge: several engine sessions join one in-memory session server and run
//! random real commands on the same drawing, often on each other's objects, while messages
//! arrive late and interleaved (in order per connection, as over a WebSocket). Once every message
//! is delivered, every client's drawing equals the server's, per seed.

mod collab_support;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use cadcraft_collab::client::carry_local;
use cadcraft_collab::message::{self, ClientMsg, ServerMsg};
use cadcraft_collab::server::{Server, To};
use cadcraft_collab::{Link, Result};
use cadcraft_engine::doc::Drawing;
use cadcraft_engine::{Session, sample};
use collab_support::{Rng, command, doc};

/// One connection's queues: what the client sent and what the server sent it, both in order.
#[derive(Default)]
struct Wire {
    up: VecDeque<ClientMsg>,
    down: VecDeque<ServerMsg>,
}

/// A link through memory. Messages go through the binary encoding, as on the network.
struct MemLink(Arc<Mutex<Wire>>);

impl Link for MemLink {
    fn send(&mut self, msg: &ClientMsg) -> Result<()> {
        let back = message::decode(&message::encode(msg)?)?;
        self.0.lock().unwrap().up.push_back(back);
        Ok(())
    }
    fn recv(&mut self) -> Option<Result<ServerMsg>> {
        self.0.lock().unwrap().down.pop_front().map(Ok)
    }
    fn is_open(&self) -> bool {
        true
    }
}

struct Member {
    session: Session,
    wire: Arc<Mutex<Wire>>,
    /// Its client id, once the server took its hello.
    id: Option<u32>,
}

struct Net {
    server: Server,
    members: Vec<Member>,
    rejected: usize,
}

impl Net {
    fn route(&mut self, out: Vec<cadcraft_collab::server::Outgoing>) {
        for o in out {
            let bytes = message::encode(&o.msg).unwrap();
            let msg: ServerMsg = message::decode(&bytes).unwrap();
            if matches!(msg, ServerMsg::Rejected { .. }) {
                self.rejected += 1;
            }
            for m in self.members.iter().filter(|m| m.id.is_some() && (o.to == To::All || o.to == To::One(m.id.unwrap_or(0)))) {
                m.wire.lock().unwrap().down.push_back(msg.clone());
            }
        }
    }

    /// Deliver the oldest message member `i` sent, if any.
    fn deliver(&mut self, i: usize) -> bool {
        let Some(msg) = self.members[i].wire.lock().unwrap().up.pop_front() else { return false };
        match msg {
            ClientMsg::Hello { name, seed, .. } => {
                let (id, out) = self.server.join(&name, seed.as_deref()).unwrap();
                self.members[i].id = Some(id);
                self.route(out);
            }
            ClientMsg::Batch { batch, changes } => {
                let id = self.members[i].id.unwrap();
                let out = self.server.batch(id, batch, changes);
                self.route(out);
            }
            ClientMsg::List => {}
        }
        true
    }

    fn join(&mut self, start: Drawing, host: bool) {
        let mut session = Session::new();
        session.open_drawing(start, "shared", None);
        let wire = Arc::new(Mutex::new(Wire::default()));
        session.collab_attach(Box::new(MemLink(wire.clone())), &format!("user{}", self.members.len()), host).unwrap();
        self.members.push(Member { session, wire, id: None });
    }

    /// Deliver everything and let every client catch up.
    fn quiesce(&mut self) {
        loop {
            let mut moved = false;
            for i in 0..self.members.len() {
                while self.deliver(i) {
                    moved = true;
                }
            }
            for m in &mut self.members {
                m.session.collab_tick();
            }
            let idle = self.members.iter().all(|m| {
                let w = m.wire.lock().unwrap();
                w.up.is_empty() && w.down.is_empty()
            });
            if idle && !moved {
                return;
            }
        }
    }
}

/// `view` with the server's copies of what never syncs, to compare with the server's drawing.
fn comparable(view: &Drawing, server: &Drawing) -> Drawing {
    let mut v = view.clone();
    carry_local(server, &mut v);
    v
}

fn run(seed: u64, clients: usize, steps: usize) -> usize {
    let mut net = Net { server: Server::new(None), members: Vec::new(), rejected: 0 };
    net.join(sample::bracket(), true);
    for _ in 1..clients {
        net.join(Drawing::default(), false);
    }
    net.quiesce();
    for m in &net.members {
        assert!(m.session.collab_joined(), "seed {seed}: a client did not join");
    }
    let mut r = Rng(seed);
    for step in 0..steps {
        let i = r.below(clients);
        match r.below(10) {
            0..=4 => {
                let s = &mut net.members[i].session;
                let (id, p) = command(&doc(s), &mut r, step * clients + i);
                let _ = s.execute(id, &p);
                assert!(s.collab_ended().is_none(), "seed {seed} step {step}: {id} ended the session: {:?}", s.collab_ended());
            }
            5..=7 => {
                for _ in 0..1 + r.below(3) {
                    net.deliver(i);
                }
            }
            _ => net.members[i].session.collab_tick(),
        }
    }
    net.quiesce();
    assert!(net.server.seq() >= steps as u64 / 5, "seed {seed}: only {} batches reached the server", net.server.seq());
    let server = net.server.drawing().unwrap().clone();
    for (k, m) in net.members.iter().enumerate() {
        assert!(m.session.collab_ended().is_none(), "seed {seed}: client {k} left: {:?}", m.session.collab_ended());
        let v = comparable(&doc(&m.session), &server);
        if v != server {
            let left = cadcraft_collab::diff(&v, &server).unwrap();
            panic!("seed {seed}: client {k} differs from the server: {left:#?}");
        }
    }
    net.rejected
}

#[test]
fn concurrent_sessions_converge() {
    let mut rejected = 0;
    for seed in 1..=24 {
        rejected += run(seed, 3, 120);
    }
    // Concurrent edits to the same objects must happen, or this test checks little.
    assert!(rejected > 0, "no batch was ever refused: the clients never conflicted");
}

#[test]
fn many_clients_converge() {
    for seed in 100..104 {
        run(seed, 6, 200);
    }
}

#[test]
fn undo_is_refused_in_a_shared_session() {
    let mut net = Net { server: Server::new(None), members: Vec::new(), rejected: 0 };
    net.join(Drawing::default(), true);
    net.quiesce();
    let s = &mut net.members[0].session;
    s.execute("line", &serde_json::json!({"points": [[0, 0], [1, 1]]})).unwrap();
    assert!(s.undo().is_err());
    assert!(s.execute("u", &serde_json::json!({})).is_err());
}
