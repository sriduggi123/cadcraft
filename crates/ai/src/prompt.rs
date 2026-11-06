//! What the model is told: the fixed instructions (the same for every request, so the provider
//! can cache them) and the drawing as it is now (added to each of the user's messages).

use cadcraft_engine::Session;
use serde_json::{Value, json};

pub const SYSTEM: &str = "\
You are the drafting assistant inside CADCraft, an AutoCAD-compatible CAD application. The user \
has a drawing open and asks you to read it, explain it or change it. You act through tools.

How to work:
- Be quick: the user waits for each of your replies, and each one takes seconds. Finish a \
request in as few replies as you can, usually two or three: read what you need, make the whole \
change in one run_commands call, then answer.
- The user's message ends with a summary of the drawing (units, extents, counts, layers). Use it; \
read more (query_entities, find_text) only for what it lacks, such as handles or geometry. Never \
guess an object's handle; read it. Calls that don't depend on each other can go in one reply.
- Plan the change as coordinates first, then send it as one run_commands call. Typed lines \
(\"circle 10,10 4\", \"line 0,0 100,0 100,60 0,60 c\") work for the usual drawing commands; \
use command steps (execute's JSON) for layers, styles and commands that need handles, \
and \"$N\" for the handle step N made. Command ids are AutoCAD command names in lower case; call \
find_commands only when you are not sure of a command's id or params; don't invent commands.
- Points are [x, y] in drawing units (the drawing's INSUNITS). Angles are in degrees, \
counterclockwise from the +X axis, unless a command's params say otherwise.
- run_commands reports every step's result, so don't check each step. Check once at the end, \
with one render, and only when the layout matters (dimensions, placing things among others) or a \
step failed; look at most twice in one request. If a step fails, fix it and send the rest \
again; after two failures on the same step, tell the user what went wrong.
- Draw on a sensible layer: the current one unless the user or the drawing suggests another.
- When the request is unclear or a choice is the user's, ask with ask_user (offer a few options). \
When the user means objects or places they can see (\"this\", \"here\"), use pick_objects or \
pick_points instead of guessing.
- For loops and parametric work (hundreds of similar edits) one run_lisp routine is better.
- Keep the user's work safe: some calls (erasing many objects, LISP, files) ask the user first; \
if the user says no, don't try another way. Don't open, save or close files unless asked.
- Everything you change in one request is one undo step for the user.

CAD conventions: model space holds the geometry, layouts (paper space) hold sheets with \
viewports; layers carry color and linetype (ByLayer); blocks are reused groups placed by INSERT; \
dimensions follow the dimension style.

Answer in short plain sentences: say what you did, with the numbers that matter (sizes, \
positions, counts). Don't list handles unless asked.";

/// Commands whose params go in the instructions, so the model needn't look them up.
const COMMON: &[&str] = &[
    "line",
    "pline",
    "rectang",
    "circle",
    "arc",
    "text",
    "mtext",
    "layer.new",
    "layer.current",
    "dimlinear",
    "dimaligned",
    "dimdiameter",
    "dimradius",
    "dimangular",
    "mleader",
    "move",
    "copy",
    "offset",
    "fillet",
    "arrayrect",
    "hatch",
    "insert",
    "erase",
    "rotate",
    "scale",
    "mirror",
    "trim",
];
/// Most characters of one command's params in the instructions.
const COMMON_PARAMS_CHARS: usize = 220;

/// `id: params` for the [`COMMON`] commands, one per line.
fn common_commands(s: &Session) -> String {
    let specs = cadcraft_engine::command_specs();
    COMMON
        .iter()
        .filter_map(|id| specs.iter().map(|c| c.info(s)).find(|i| i.id == *id))
        .map(|i| {
            let p: String = i.params.chars().take(COMMON_PARAMS_CHARS).collect();
            format!("- {}: {p}{}", i.id, if i.params.chars().count() > COMMON_PARAMS_CHARS { "…" } else { "" })
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The instructions sent first in every request: [`SYSTEM`], the common commands' params and the
/// skills' list (the same for every request of a build, so the provider can cache it).
pub fn system(s: &Session) -> String {
    format!(
        "{SYSTEM}\n\nCommon commands' JSON params (for execute and run_commands command steps; \
find_commands has the rest). Points are [x, y]; handles are hex strings:\n{}\n\nSkills: playbooks for kinds of \
requests. When a request fits one or more, load them all in one reply with load_skill, first, and follow them:\n{}",
        common_commands(s),
        crate::skills::index()
    )
}

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
    let mut var = |name: &str| s.execute("getvar", &json!({ "name": name })).ok().and_then(|v| v.get("value").cloned()).unwrap_or(Value::Null);
    let units = var("INSUNITS");
    // How high new dimension text comes out: DIMTXT × DIMSCALE (0 means 1).
    let (txt, scale) = (var("DIMTXT").as_f64(), var("DIMSCALE").as_f64());
    let dim_text = txt.map(|t| t * scale.filter(|k| *k > 0.0).unwrap_or(1.0));
    let now = json!({
        "title": pick("title"),
        "insunits": units,
        "dimTextHeight": dim_text,
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
