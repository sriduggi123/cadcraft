//! Shared sessions converge: several engine sessions join one in-memory session server and run
//! random real commands on the same drawing, often on each other's objects, while messages
//! arrive late and interleaved (in order per connection, as over a WebSocket). Once every message
//! is delivered, every client's drawing equals the server's, per seed.

mod collab_support;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use cadcraft_collab::client::carry_local;
use cadcraft_collab::message::{self, ClientMsg, ServerMsg};
use cadcraft_collab::server::Server;
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
    /// Undos and redos that changed a drawing.
    undone: usize,
}

impl Net {
    fn route(&mut self, out: Vec<cadcraft_collab::server::Outgoing>) {
        for o in out {
            let bytes = message::encode(&o.msg).unwrap();
            let msg: ServerMsg = message::decode(&bytes).unwrap();
            if matches!(msg, ServerMsg::Rejected { .. }) {
                self.rejected += 1;
            }
            for m in self.members.iter().filter(|m| m.id.is_some() && o.to.reaches(m.id.unwrap_or(0))) {
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
            ClientMsg::Batch { batch, base, changes } => {
                let id = self.members[i].id.unwrap();
                let out = self.server.batch(id, batch, base, changes);
                self.route(out);
            }
            ClientMsg::List => {}
            ClientMsg::Lock { block, on } => {
                let id = self.members[i].id.unwrap();
                let out = self.server.lock(id, &block, on);
                self.route(out);
            }
            ClientMsg::Presence(p) => {
                let id = self.members[i].id.unwrap();
                let out = self.server.presence(id, p);
                self.route(out);
            }
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
    let mut net = Net { server: Server::new(None), members: Vec::new(), rejected: 0, undone: 0 };
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
        match r.below(12) {
            0..=4 => {
                let s = &mut net.members[i].session;
                let (id, p) = command(&doc(s), &mut r, step * clients + i);
                let _ = s.execute(id, &p);
                assert!(s.collab_ended().is_none(), "seed {seed} step {step}: {id} ended the session: {:?}", s.collab_ended());
            }
            // Per-person undo and redo, mixed in with everyone's edits.
            5 | 6 => {
                let s = &mut net.members[i].session;
                let id = if r.below(3) == 0 { "redo" } else { "u" };
                let before = doc(s);
                let _ = s.execute(id, &serde_json::json!({}));
                if !Arc::ptr_eq(&before, &doc(s)) {
                    net.undone += 1;
                }
                assert!(s.collab_ended().is_none(), "seed {seed} step {step}: {id} ended the session: {:?}", s.collab_ended());
            }
            7..=9 => {
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
    assert!(net.undone > 0, "seed {seed}: no undo or redo changed anything");
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

fn count(s: &Session, kind: &str) -> usize {
    doc(s).model.iter().filter(|e| e.kind.dxf_name() == kind).count()
}

/// U takes back only its author's step: someone else's later edit stays, on every copy. REDO
/// brings it back. A change someone else made since is left as they made it.
#[test]
fn undo_takes_back_only_your_own_steps() {
    use serde_json::json;
    let mut net = Net { server: Server::new(None), members: Vec::new(), rejected: 0, undone: 0 };
    net.join(Drawing::default(), true);
    net.join(Drawing::default(), false);
    net.quiesce();
    net.members[0].session.execute("circle", &json!({"center": [0, 0], "radius": 5})).unwrap();
    net.quiesce();
    net.members[1].session.execute("line", &json!({"points": [[0, 0], [10, 0]]})).unwrap();
    net.quiesce();

    net.members[0].session.execute("u", &json!({})).unwrap();
    net.quiesce();
    for m in &net.members {
        assert_eq!((count(&m.session, "CIRCLE"), count(&m.session, "LINE")), (0, 1), "the circle is undone, Bo's line stays");
    }
    net.members[0].session.execute("redo", &json!({})).unwrap();
    net.quiesce();
    for m in &net.members {
        assert_eq!((count(&m.session, "CIRCLE"), count(&m.session, "LINE")), (1, 1));
    }

    // Asha colours the line red; Bo then makes it blue. Asha's undo leaves Bo's blue.
    let line = doc(&net.members[0].session).model.iter().find(|e| e.kind.dxf_name() == "LINE").map(|e| e.handle.hex()).unwrap();
    net.members[0].session.execute("properties.set", &json!({"handles": [line], "color": 1})).unwrap();
    net.quiesce();
    net.members[1].session.execute("properties.set", &json!({"handles": [line], "color": 5})).unwrap();
    net.quiesce();
    net.members[0].session.execute("u", &json!({})).unwrap();
    net.quiesce();
    let colour = |s: &Session| doc(s).model.iter().find(|e| e.handle.hex() == line).map(|e| format!("{:?}", e.common.color)).unwrap();
    let blue = colour(&net.members[1].session);
    assert_eq!(colour(&net.members[0].session), blue);
    assert!(net.members[0].session.log.iter().any(|l| l.contains("left as they are")), "Asha is told");
    assert_eq!(net.server.drawing().map(|d| d.model.len()), Some(2));
}

/// An UNDO BEgin/End group and a LISP routine each undo as one step.
#[test]
fn a_group_and_a_lisp_routine_undo_as_one_step() {
    use serde_json::json;
    let mut net = Net { server: Server::new(None), members: Vec::new(), rejected: 0, undone: 0 };
    net.join(Drawing::default(), true);
    net.join(Drawing::default(), false);
    net.quiesce();
    let a = &mut net.members[0].session;
    a.execute("undo", &json!({"mode": "begin"})).unwrap();
    a.execute("circle", &json!({"center": [0, 0], "radius": 5})).unwrap();
    net.quiesce();
    // Bo draws in the middle of Asha's group.
    net.members[1].session.execute("line", &json!({"points": [[0, 0], [10, 0]]})).unwrap();
    net.quiesce();
    let a = &mut net.members[0].session;
    a.execute("circle", &json!({"center": [20, 0], "radius": 5})).unwrap();
    a.execute("undo", &json!({"mode": "end"})).unwrap();
    a.execute("u", &json!({})).unwrap();
    net.quiesce();
    for m in &net.members {
        assert_eq!((count(&m.session, "CIRCLE"), count(&m.session, "LINE")), (0, 1), "both circles go, Bo's line stays");
    }

    let a = &mut net.members[0].session;
    a.cmdline("(progn (command \"circle\" \"0,0\" \"3\") (command \"circle\" \"9,0\" \"3\"))").unwrap();
    while a.lisp_busy() {
        a.lisp_tick();
    }
    net.quiesce();
    assert_eq!(count(&net.members[1].session, "CIRCLE"), 2);
    net.members[0].session.execute("u", &json!({})).unwrap();
    net.quiesce();
    for m in &net.members {
        assert_eq!((count(&m.session, "CIRCLE"), count(&m.session, "LINE")), (0, 1), "the routine's two circles undo together");
    }
}

fn two_people() -> Net {
    let mut net = Net { server: Server::new(None), members: Vec::new(), rejected: 0, undone: 0 };
    net.join(Drawing::default(), true);
    net.join(Drawing::default(), false);
    net.quiesce();
    net
}

fn dim_points(s: &Session) -> ([f64; 2], [f64; 2]) {
    let d = doc(s);
    let dm = d
        .model
        .iter()
        .find_map(|e| match &e.kind {
            cadcraft_doc::EntityKind::Dimension(dm) => Some(dm.clone()),
            _ => None,
        })
        .unwrap();
    ([dm.p13.x, dm.p13.y], [dm.p14.x, dm.p14.y])
}

/// P5: Asha moves a line while Bo, at the same moment, moves its dimension: Bo's dimension
/// lands last with the line's old ends, and the repair puts it back on the line, everywhere.
/// A dimension moved off its line by itself, with nobody editing at the same time, stays put.
#[test]
fn a_dimension_broken_by_two_edits_at_once_is_repaired() {
    use serde_json::json;
    let mut net = two_people();
    let a = &mut net.members[0].session;
    a.execute("line", &json!({"points": [[0, 0], [10, 0]]})).unwrap();
    a.execute("dimlinear", &json!({"p1": [0, 0], "p2": [10, 0], "at": [5, 4]})).unwrap();
    net.quiesce();
    let handles: Vec<String> = doc(&net.members[0].session).model.iter().map(|e| e.handle.hex()).collect();
    let (line, dim) = (handles[0].clone(), handles[1].clone());
    assert_eq!(dim_points(&net.members[1].session), ([0.0, 0.0], [10.0, 0.0]));

    // Both edit before either sees the other's edit; Asha's reaches the server first.
    net.members[0].session.execute("move", &json!({"handles": [line], "delta": [0, -3]})).unwrap();
    net.members[1].session.execute("move", &json!({"handles": [dim], "delta": [1, 1]})).unwrap();
    net.deliver(0);
    net.deliver(1);
    net.quiesce();
    for m in &net.members {
        assert_eq!(dim_points(&m.session), ([0.0, -3.0], [10.0, -3.0]), "the dimension measures the moved line again");
    }

    // Bo moves the dimension alone: nothing is repaired.
    net.members[1].session.execute("move", &json!({"handles": [dim], "delta": [0, 2]})).unwrap();
    net.quiesce();
    for m in &net.members {
        assert_eq!(dim_points(&m.session), ([0.0, -1.0], [10.0, -1.0]));
    }
}

/// P5: Asha renames layer Walls while Bo draws on it: Bo's line ends up on the new name, and no
/// empty Walls layer comes back. Two people changing different layers keep both changes.
#[test]
fn tables_sync_by_entry_and_drawing_follows_a_rename() {
    use serde_json::json;
    let mut net = two_people();
    net.members[0].session.execute("layer.new", &json!({"name": "Walls", "color": 1})).unwrap();
    net.members[0].session.execute("layer.new", &json!({"name": "Doors", "color": 2})).unwrap();
    net.quiesce();
    net.members[1].session.execute("layer.current", &json!({"name": "Walls"})).unwrap();

    net.members[0].session.execute("rename", &json!({"table": "layer", "from": "Walls", "to": "Walls-2"})).unwrap();
    net.members[1].session.execute("line", &json!({"points": [[0, 0], [10, 0]]})).unwrap();
    net.deliver(0);
    net.deliver(1);
    net.quiesce();
    for m in &net.members {
        let d = doc(&m.session);
        assert!(d.layer("Walls").is_none(), "no Walls layer comes back");
        assert_eq!(d.model.iter().map(|e| e.common.layer.clone()).collect::<Vec<_>>(), vec!["Walls-2".to_string()]);
    }

    // Different layers at once: both changes stay.
    net.members[0].session.execute("layer.set", &json!({"name": "Walls-2", "color": 5})).unwrap();
    net.members[1].session.execute("layer.set", &json!({"name": "Doors", "color": 6})).unwrap();
    let colour = |s: &Session, n: &str| doc(s).layer(n).map(|l| format!("{:?}", l.color)).unwrap();
    let want = (colour(&net.members[0].session, "Walls-2"), colour(&net.members[1].session, "Doors"));
    net.quiesce();
    for m in &net.members {
        assert_eq!((colour(&m.session, "Walls-2"), colour(&m.session, "Doors")), want, "both layer changes stay");
    }
}

fn block_len(s: &Session, name: &str) -> usize {
    s.state().unwrap().drawing().block(name).map_or(0, |b| b.entities.len())
}

/// P5: the Block Editor locks its block. Someone else can't open it meanwhile, and their other
/// edits (a layer change) arrive and survive Asha's save; after BCLOSE the lock is gone.
#[test]
fn the_block_editor_locks_its_block() {
    use serde_json::json;
    let mut net = two_people();
    let a = &mut net.members[0].session;
    a.execute("circle", &json!({"center": [0, 0], "radius": 1})).unwrap();
    let h = doc(a).model.iter().next().map(|e| e.handle.hex()).unwrap();
    a.execute("block", &json!({"name": "Bolt", "base": [0, 0], "handles": [h]})).unwrap();
    a.execute("layer.new", &json!({"name": "Walls", "color": 1})).unwrap();
    net.quiesce();

    net.members[0].session.execute("bedit", &json!({"name": "Bolt"})).unwrap();
    net.members[0].session.execute("line", &json!({"points": [[0, 0], [2, 2]]})).unwrap();
    net.quiesce();
    assert_eq!(block_len(&net.members[1].session, "Bolt"), 1, "nothing leaves the editor before a save");

    // Bo can't open Bolt; his layer change arrives behind Asha's editor.
    let b = &mut net.members[1].session;
    let _ = b.execute("bedit", &json!({"name": "Bolt"}));
    b.collab_tick();
    assert!(b.state().unwrap().block_edit.is_none(), "Bo's editor closed again");
    assert!(b.log.iter().any(|l| l.contains("being edited by user0")), "{:?}", b.log);
    b.execute("layer.set", &json!({"name": "Walls", "color": 5})).unwrap();
    let blue = doc(b).layer("Walls").map(|l| format!("{:?}", l.color)).unwrap();
    net.quiesce();

    net.members[0].session.execute("bclose", &json!({"save": true})).unwrap();
    net.quiesce();
    for m in &net.members {
        assert_eq!(block_len(&m.session, "Bolt"), 2, "the saved block reaches everyone");
        assert_eq!(doc(&m.session).layer("Walls").map(|l| format!("{:?}", l.color)), Some(blue.clone()), "Bo's layer change survives the save");
    }

    // The lock is gone: Bo can edit Bolt now.
    net.members[1].session.execute("bedit", &json!({"name": "Bolt"})).unwrap();
    net.members[1].session.collab_tick();
    assert!(net.members[1].session.state().unwrap().block_edit.is_some());
}
