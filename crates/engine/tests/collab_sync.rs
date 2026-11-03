//! Collaboration diffs on real commands: after every command, the changes `cadcraft_collab::diff`
//! finds, sent through the wire encodings and applied to a second copy of the drawing, make that
//! copy equal to the edited drawing (per-person variables and session-only data aside). Random
//! command sequences per seed, on a blank drawing and on the samples; undo and redo included.

use std::collections::BTreeSet;
use std::sync::Arc;

mod collab_support;

use cadcraft_collab::{Change, apply, diff, snapshot, vars};
use cadcraft_engine::doc::Drawing;
use cadcraft_engine::{Session, sample};
use collab_support::{Rng, command, doc};
use serde_json::json;

/// `replica` with the parts of `real` that never sync copied over: per-person variables, the
/// handle seed and session-only data.
fn comparable(replica: &Drawing, real: &Drawing) -> Drawing {
    let mut r = replica.clone();
    r.handseed = real.handseed;
    r.header.vars.retain(|k, _| !vars::is_local(k));
    r.header.vars.extend(real.header.vars.iter().filter(|(k, _)| vars::is_local(k)).map(|(k, v)| (k.clone(), v.clone())));
    r.hidden_objects = real.hidden_objects.clone();
    r.read_notes = real.read_notes.clone();
    r.refedit = real.refedit.clone();
    r.skipped_records = real.skipped_records.clone();
    r.portable_fonts = real.portable_fonts;
    r
}

/// Send a batch both ways a server would: as JSON and as postcard.
fn over_the_wire(changes: &[Change]) -> Vec<Change> {
    let text = serde_json::to_string(changes).unwrap();
    let from_json: Vec<Change> = serde_json::from_str(&text).unwrap();
    let bytes = postcard::to_allocvec(&from_json).unwrap();
    postcard::from_bytes(&bytes).unwrap()
}

/// The kind of a change, as its JSON form names it (`create`, `order`…).
fn kind(c: &Change) -> String {
    let v = serde_json::to_value(c).unwrap();
    v.as_object().and_then(|o| o.keys().next().cloned()).unwrap_or_default()
}

/// Apply what changed between `before` and `after` to `replica`, check it caught up, and return
/// the kinds of change the batch held.
fn sync(replica: &mut Drawing, before: &Drawing, after: &Drawing, what: &str) -> BTreeSet<String> {
    let changes = over_the_wire(&diff(before, after).unwrap());
    if let Err(e) = apply(replica, &changes) {
        panic!("{what}: applying {changes:#?} failed: {e}");
    }
    let r = comparable(replica, after);
    if r != *after {
        let left = diff(&r, after).unwrap();
        panic!("{what}: the copy differs after {changes:#?}\nstill to change: {left:#?}");
    }
    changes.iter().map(kind).collect()
}

/// Run `steps` random commands from `start`, syncing a copy after each; returns the kinds of
/// change seen.
fn run(seed: u64, start: Drawing, steps: usize) -> BTreeSet<String> {
    let mut s = Session::new();
    s.open_drawing(start, "collab", None);
    let mut replica = (*doc(&s)).clone();
    let mut r = Rng(seed);
    let mut seen = BTreeSet::new();
    for step in 0..steps {
        let before = doc(&s);
        let what = match r.below(12) {
            0 => {
                let _ = s.undo();
                "undo".to_string()
            }
            1 => {
                let _ = s.redo();
                "redo".to_string()
            }
            _ => {
                let (id, p) = command(&before, &mut r, step);
                let _ = s.execute(id, &p);
                format!("seed {seed} step {step}: {id} {p}")
            }
        };
        let after = doc(&s);
        if !Arc::ptr_eq(&before, &after) {
            seen.extend(sync(&mut replica, &before, &after, &what));
        }
    }
    let end = doc(&s);
    let back = snapshot::decode(&snapshot::encode(&end).unwrap()).unwrap();
    assert!(comparable(&back, &end) == *end, "seed {seed}: snapshot round trip differs");
    seen
}

#[test]
fn every_command_syncs_on_a_blank_drawing() {
    let mut seen = BTreeSet::new();
    for seed in 1..=12 {
        seen.extend(run(seed, Drawing::default(), 80));
    }
    // The commands must keep reaching every kind of change, or this test checks little.
    for k in ["delete", "block", "create", "common", "geometry", "order", "var", "entry"] {
        assert!(seen.contains(k), "no `{k}` change in any run: {seen:?}");
    }
}

#[test]
fn every_command_syncs_on_the_samples() {
    for seed in 100..106 {
        run(seed, sample::bracket(), 60);
        run(seed + 50, sample::floor_plan(), 60);
    }
}

#[test]
fn an_unchanged_drawing_has_no_changes() {
    let d = sample::floor_plan();
    assert!(diff(&d, &d.clone()).unwrap().is_empty());
}

#[test]
fn per_person_variables_never_sync() {
    let mut s = Session::new();
    s.open_drawing(Drawing::default(), "collab", None);
    s.execute("layer.new", &json!({"name": "WALLS"})).unwrap();
    let before = doc(&s);
    s.execute("layer.current", &json!({"name": "WALLS"})).unwrap();
    let after = doc(&s);
    let changes = diff(&before, &after).unwrap();
    assert!(!changes.iter().any(|c| matches!(c, Change::Var { name, .. } if name == "CLAYER")), "{changes:?}");
}

/// Handles a drawing refers to: groups' members and associative dimensions' objects.
fn references(d: &Drawing) -> Vec<cadcraft_engine::doc::Handle> {
    let groups = d.groups.iter().flat_map(|g| g.members.iter().copied());
    let dims = d.model.iter().filter_map(|e| match &e.kind {
        cadcraft_engine::doc::EntityKind::Dimension(dim) => Some(dim.assoc.iter().map(|a| a.handle).collect::<Vec<_>>()),
        _ => None,
    });
    groups.chain(dims.flatten()).collect()
}

#[test]
fn session_handles_are_renumbered_in_dwg_and_dxf() {
    let mut start = sample::floor_plan();
    cadcraft_collab::space::enter(&mut start, 9);
    let mut s = Session::new();
    s.open_drawing(start, "collab", None);
    let line = s.execute("line", &json!({"points": [[0, 0], [40, 0]]})).unwrap();
    let h = doc(&s).model.last().map(|e| e.handle).unwrap();
    assert_eq!(cadcraft_collab::space::space_of(h), 9, "{line}");
    s.execute("dimlinear", &json!({"object": h.hex(), "at": [20, -8]})).unwrap();
    s.execute("circle", &json!({"center": [5, 5], "radius": 2})).unwrap();
    let c = doc(&s).model.last().map(|e| e.handle).unwrap();
    s.execute("group", &json!({"name": "PAIR", "handles": [h.hex(), c.hex()]})).unwrap();
    s.execute("circle", &json!({"center": [60, 5], "radius": 3})).unwrap();
    let b = doc(&s).model.last().map(|e| e.handle).unwrap();
    s.execute("block", &json!({"name": "KNOB", "base": [60, 5], "handles": [b.hex()]})).unwrap();
    s.execute("insert", &json!({"name": "KNOB", "at": [80, 5]})).unwrap();
    let d = doc(&s);
    assert!(!references(&d).is_empty());
    for name in ["x.dxf", "x.dwg"] {
        let bytes = cadcraft_io::write_shared(&d, name, &Default::default()).unwrap();
        let back = cadcraft_io::read(&bytes, name).unwrap();
        let all: Vec<u64> = back.model.iter().map(|e| e.handle.0).collect();
        assert!(all.iter().all(|h| *h < 1 << 32), "{name}: {all:X?}");
        assert!(back.handseed < 1 << 32, "{name}: seed {:X}", back.handseed);
        assert_eq!(back.model.len(), d.model.len(), "{name}");
        assert!(back.blocks.get("KNOB").is_some_and(|k| k.entities.len() == 1), "{name}");
        let refs = references(&back);
        assert_eq!(refs.len(), references(&d).len(), "{name}");
        assert!(refs.iter().all(|h| back.model.contains(*h)), "{name}: dangling {refs:X?}");
    }
}
