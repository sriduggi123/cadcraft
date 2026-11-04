//! What the model is told: the fixed instructions (the same for every request, so the provider
//! can cache them) and the drawing as it is now (added to each of the user's messages).

use cadcraft_engine::Session;
use serde_json::{Value, json};

pub const SYSTEM: &str = "\
You are the drafting assistant inside CADCraft, an AutoCAD-compatible CAD application. The user \
has a drawing open and asks you to read it, explain it or change it. You act through tools.

How to work:
- Look before you change: use drawing_summary, query_entities and find_text to learn the units, \
extents, layers and the objects involved. Never guess an object's handle; read it.
- Change the drawing with execute. Command ids are AutoCAD command names in lower case. When you \
are not sure of a command's id or params, call find_commands first; don't invent commands.
- Points are [x, y] in drawing units (the drawing's INSUNITS). Angles are in degrees, \
counterclockwise from the +X axis, unless a command's params say otherwise.
- After changing something, check the result (query_entities or drawing_summary) before \
saying it is done. If a command fails, read the error, fix the params and try again; after \
two failures on the same step, tell the user what went wrong.
- Draw on a sensible layer: the current one unless the user or the drawing suggests another.
- Keep the user's work safe: ask before erasing many objects or anything the user didn't \
mention. Don't open, save or close files.
- Everything you change in one request is one undo step for the user.

CAD conventions: model space holds the geometry, layouts (paper space) hold sheets with \
viewports; layers carry color and linetype (ByLayer); blocks are reused groups placed by INSERT; \
dimensions follow the dimension style.

Answer in short plain sentences: say what you did, with the numbers that matter (sizes, \
positions, counts). Don't list handles unless asked.";

/// The drawing now, compact: title, units, extents, counts, layers, current layer, selection.
pub fn drawing_now(s: &mut Session) -> String {
    let summary = s.execute("drawing.inspect", &json!({"entities": false})).unwrap_or(Value::Null);
    let pick = |k: &str| summary.get(k).cloned().unwrap_or(Value::Null);
    let layers: Vec<Value> = summary
        .get("layers")
        .and_then(Value::as_array)
        .map(|l| l.iter().take(60).map(|l| l.get("name").cloned().unwrap_or_else(|| l.clone())).collect())
        .unwrap_or_default();
    // INSUNITS: 0 none, 1 inches, 2 feet, 4 millimeters, 5 centimeters, 6 meters.
    let units = s.execute("getvar", &json!({"name": "INSUNITS"})).ok().and_then(|v| v.get("value").cloned()).unwrap_or(Value::Null);
    let now = json!({
        "title": pick("title"),
        "insunits": units,
        "extents": pick("extents"),
        "counts": pick("counts"),
        "layers": layers,
        "currentLayer": pick("currentLayer"),
        "space": pick("space"),
        "selectionCount": s.selection().len(),
        "running": s.running.as_ref().map(|r| r.id.clone()),
    });
    serde_json::to_string(&now).unwrap_or_default()
}
