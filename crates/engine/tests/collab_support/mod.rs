//! Shared by the collaboration tests: a deterministic generator and random real commands.
#![allow(dead_code)]

use std::sync::Arc;

use cadcraft_engine::Session;
use cadcraft_engine::doc::Drawing;
use serde_json::{Value, json};

/// A small deterministic generator (no dependency for tests).
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
    pub fn coord(&mut self) -> f64 {
        (self.below(2000) as f64) / 10.0 - 100.0
    }
    pub fn pt(&mut self) -> Value {
        json!([self.coord(), self.coord()])
    }
}

pub fn doc(s: &Session) -> Arc<Drawing> {
    s.state().map(|st| st.doc.clone()).unwrap()
}

/// Some model-space handles, as the commands take them.
fn some_handles(d: &Drawing, r: &mut Rng) -> Vec<String> {
    let all = d.model.handles();
    if all.is_empty() {
        return Vec::new();
    }
    (0..1 + r.below(3)).map(|_| all[r.below(all.len())].hex()).collect()
}

fn layer_name(d: &Drawing, r: &mut Rng) -> String {
    d.layers[r.below(d.layers.len())].name.clone()
}

/// One random command with plausible params.
pub fn command(d: &Drawing, r: &mut Rng, step: usize) -> (&'static str, Value) {
    let h = some_handles(d, r);
    match r.below(22) {
        0 | 1 => ("line", json!({"points": [r.pt(), r.pt(), r.pt()]})),
        2 => ("circle", json!({"center": r.pt(), "radius": 1 + r.below(20)})),
        3 => ("arc", json!({"center": r.pt(), "radius": 1 + r.below(20), "start": r.below(360), "end": r.below(360)})),
        4 => ("text", json!({"at": r.pt(), "text": format!("T{step}"), "height": 2.5})),
        5 => ("pline", json!({"vertices": [r.pt(), {"p": r.pt(), "bulge": 0.5}, r.pt()], "closed": r.below(2) == 1})),
        6 => ("move", json!({"handles": h, "delta": r.pt()})),
        7 => ("copy", json!({"handles": h, "delta": r.pt()})),
        8 => ("rotate", json!({"handles": h, "base": r.pt(), "angle": r.below(360)})),
        9 => ("scale", json!({"handles": h, "base": r.pt(), "factor": 0.5 + r.below(3) as f64})),
        10 => ("mirror", json!({"handles": h, "p1": r.pt(), "p2": r.pt(), "erase": r.below(2) == 1})),
        11 => ("erase", json!({"handles": h})),
        12 => ("layer.new", json!({"name": format!("L{step}"), "color": 1 + r.below(7), "current": r.below(2) == 1})),
        13 => ("properties.set", json!({"handles": h, "layer": layer_name(d, r), "color": 1 + r.below(7)})),
        14 => ("block", json!({"name": format!("B{step}"), "base": r.pt(), "handles": h})),
        15 => match d.blocks.keys().find(|n| !n.starts_with('*')) {
            Some(n) => ("insert", json!({"name": n, "at": r.pt(), "rotation": r.below(360)})),
            None => ("circle", json!({"center": r.pt(), "radius": 3})),
        },
        16 => ("explode", json!({"handles": h})),
        17 => ("draworder", json!({"handles": h, "order": if r.below(2) == 1 { "front" } else { "back" }})),
        18 => ("group", json!({"handles": h})),
        19 => ("dimlinear", json!({"p1": r.pt(), "p2": r.pt(), "at": r.pt()})),
        20 => ("layer.set", json!({"name": layer_name(d, r), "color": 1 + r.below(7), "frozen": false})),
        _ => ("layer.current", json!({"name": layer_name(d, r)})),
    }
}
