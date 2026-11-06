//! The tools the model may call, as OpenAI function definitions, and running them on the session.
//! A few general tools, not one per command: `find_commands` searches the catalog and `execute`
//! runs any command, so every command is reachable without sending 460 definitions each time.

use cadcraft_engine::Session;
use serde_json::{Value, json};

/// Most characters of one tool result sent back to the model.
pub const MAX_RESULT_CHARS: usize = 16_000;
/// Most commands one `find_commands` returns.
const MAX_FOUND: usize = 20;
/// Default page size for entity and text queries.
const PAGE: u64 = 100;
/// Most steps one `run_commands` call may have.
pub const MAX_STEPS: usize = 200;
/// Most characters of one step's result in `run_commands`' answer.
const STEP_RESULT_CHARS: usize = 400;

fn window() -> Value {
    json!({"type": "array", "description": "[[x0,y0],[x1,y1]] in drawing units", "items": {"type": "array", "items": {"type": "number"}}})
}

fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({"type": "function", "function": {"name": name, "description": description,
        "parameters": {"type": "object", "properties": properties, "required": required}}})
}

/// The tool definitions sent with every request.
pub fn definitions() -> Value {
    json!([
        tool(
            "drawing_summary",
            "Summary of the open drawing: units, extents, entity counts by type, layers, styles, blocks, layouts, current layer and space, selection, undo history.",
            json!({}),
            &[]
        ),
        tool(
            "query_entities",
            "Entities with handles and full geometry, filtered by type (Line, Circle, Arc, Polyline, Text, MText, Dimension, Insert, Hatch…), layer, the current selection (selected:true) and/or a window (crossing:false = only entities entirely inside). Paged: count is all matches, returned this page.",
            json!({"type": {"type": "string"}, "layer": {"type": "string"}, "selected": {"type": "boolean"}, "window": window(), "crossing": {"type": "boolean"},
                "offset": {"type": "integer"}, "limit": {"type": "integer"}}),
            &[]
        ),
        tool(
            "find_text",
            "The drawing's texts (TEXT, MTEXT without formatting, attributes, multileaders) with text, insertion point, height, layer and handle. Filter by contains (any case), layer, window.",
            json!({"contains": {"type": "string"}, "layer": {"type": "string"}, "window": window(), "offset": {"type": "integer"}, "limit": {"type": "integer"}}),
            &[]
        ),
        tool(
            "find_commands",
            "Search CADCraft's commands by words (e.g. \"circle\", \"fillet\", \"layer new\", \"dimension linear\", \"area\"). Returns each command's id, label, aliases and its JSON params. Use it before execute when unsure of a command's id or params.",
            json!({"query": {"type": "string"}}),
            &["query"]
        ),
        tool(
            "execute",
            "Run one CADCraft command with JSON params (no prompts). Command ids are AutoCAD command names in lower case (line, circle, move, copy, erase, layer.new, dimlinear…); find_commands gives the params. Points are [x, y] or [x, y, z] in drawing units; objects are given by handle strings.",
            json!({"command": {"type": "string"}, "params": {"type": "object"}}),
            &["command"]
        ),
        tool(
            "run_commands",
            "Run many steps in one call, in order: the fast way to make a change, since each reply \
             the user waits for costs seconds. A step is {\"command\": id, \"params\": {…}} (like execute) or \
             {\"line\": text} (like command_line, e.g. \"circle 10,10 4\"). A param string \"$3\" is the handle \
             command step 3 made (steps count from 1; typed lines return no handle), so a later step can dimension or move what an earlier one drew. \
             Stops at the first step that fails; returns each step's result.",
            json!({"steps": {"type": "array", "items": {"type": "object"}, "description": "1 to 200 steps"}}),
            &["steps"]
        ),
        tool(
            "command_line",
            "Type one line at the AutoCAD-style command line exactly as a user would: `circle 0,0 5`, `line 0,0 10,0 10,5 c`, `@10<45`, keywords, empty text = Enter. Returns the prompt and the command line's output. Use execute when you can; this is for input that is easier typed.",
            json!({"text": {"type": "string"}}),
            &["text"]
        ),
        tool(
            "app_state",
            "What the user is doing now: the command running and its prompt, the selection (handles), recent command-line history.",
            json!({}),
            &[]
        ),
        tool(
            "load_skill",
            "Read a skill (a playbook for a kind of request, listed in the instructions) before doing the work.",
            json!({"name": {"type": "string"}}),
            &["name"]
        ),
        tool(
            "render",
            "A picture of the drawing (model space or the current layout) to look at: the whole drawing, or a window [[x0,y0],[x1,y1]] in drawing units to see part of it closer. Use it to understand a layout, or to check what you drew looks right.",
            json!({"window": window()}),
            &[]
        ),
        tool(
            "run_lisp",
            "Run an AutoLISP routine (AutoCAD's LISP: entget, entmod, ssget \"X\" filters, command, polar, repeat, foreach…) for repeated or parametric work that would take many execute calls. Returns its value and what it printed. The user is asked before it runs.",
            json!({"code": {"type": "string"}}),
            &["code"]
        ),
        tool(
            "ask_user",
            "Ask the user a question when the request is unclear or a choice is theirs (sizes, which layer, which of two objects). options: a few short suggested answers; the user may type another.",
            json!({"question": {"type": "string"}, "options": {"type": "array", "items": {"type": "string"}}}),
            &["question"]
        ),
        tool(
            "pick_points",
            "Ask the user to click points in the drawing (with object snaps), e.g. where to place something. Returns the points [x, y, z].",
            json!({"prompt": {"type": "string", "description": "what to click, e.g. \"Pick the door's hinge point\""}, "count": {"type": "integer", "description": "1 to 20"}}),
            &["prompt"]
        ),
        tool(
            "pick_objects",
            "Ask the user to select objects in the drawing; returns their handles. Use it when the user says \"these\" or \"this one\" and the selection is empty.",
            json!({"prompt": {"type": "string"}}),
            &["prompt"]
        ),
    ])
}

/// Tools the assistant answers by waiting for the user ([`crate::Need`]), not by [`run`].
pub const USER_TOOLS: &[&str] = &["ask_user", "pick_points", "pick_objects"];

/// Commands the user is asked about first: they open, close, save, export or delete files,
/// reach the network, or remove named things from the drawing for good.
const ASK_FIRST: &[&str] = &[
    "open",
    "new",
    "qnew",
    "qsave",
    "save",
    "saveas",
    "close",
    "closeall",
    "quit",
    "exit",
    "export",
    "plot",
    "publish",
    "wblock",
    "purge",
    "recover",
    "appload",
    "lisp",
    "load",
    "xattach",
    "xref",
    "etransmit",
    "archive",
];
/// Erasing more objects than this at once asks first.
pub const ERASE_WITHOUT_ASKING: usize = 20;

/// The command a command-line text starts (its first word, `_` and `.` prefixes taken off, an
/// alias turned into its command), when no command is running.
fn typed_command(s: &Session, text: &str) -> Option<String> {
    if s.running.is_some() || s.lisp_running() {
        return None;
    }
    let word = text.split_whitespace().next()?.trim_start_matches(['_', '.', '-']).to_ascii_lowercase();
    let spec =
        cadcraft_engine::command_specs().iter().find(|c| c.info(s).id == word || c.info(s).aliases.iter().any(|a| a.eq_ignore_ascii_case(&word)));
    Some(spec.map(|c| c.info(s).id.to_string()).unwrap_or(word))
}

fn gated(id: &str) -> bool {
    ASK_FIRST.contains(&id) || id.starts_with("cloud.") || id.starts_with("collab.")
}

/// Why the user must say yes before this call runs (shown to them), or `None` to run it.
/// `lisp_ok`: the user allowed LISP routines for this chat.
pub fn permission(s: &Session, name: &str, args: &Value, lisp_ok: bool) -> Option<String> {
    match name {
        "run_lisp" if !lisp_ok => {
            let code: String = str_arg(args, "code").chars().take(600).collect();
            Some(format!("run this LISP routine:\n{code}"))
        }
        "execute" => {
            let id = str_arg(args, "command").trim().to_ascii_lowercase();
            let params = args.get("params").cloned().unwrap_or(Value::Null);
            if gated(&id) {
                return Some(format!("run {} {}", id.to_ascii_uppercase(), serde_json::to_string(&params).unwrap_or_default()));
            }
            if id == "erase" {
                let n = params.get("handles").and_then(Value::as_array).map_or_else(|| s.selection().len(), Vec::len);
                return (n > ERASE_WITHOUT_ASKING).then(|| format!("erase {n} objects"));
            }
            None
        }
        "command_line" => {
            let text = str_arg(args, "text");
            let id = typed_command(s, text)?;
            (gated(&id) || id == "erase").then(|| format!("type `{}` at the command line", text.trim()))
        }
        "run_commands" => {
            // One question for the whole batch: every step that would ask on its own.
            let asks: Vec<String> = steps(args).iter().filter_map(|(name, a)| permission(s, name, a, lisp_ok)).collect();
            (!asks.is_empty()).then(|| asks.join("\n"))
        }
        _ => None,
    }
}

/// A `run_commands` call's steps as the single-step tool each one is (`execute` or
/// `command_line`) and its arguments. Steps that are neither are kept as a tool that fails.
fn steps(args: &Value) -> Vec<(&'static str, Value)> {
    let list = args.get("steps").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
    list.iter()
        .map(|step| {
            if let Some(line) = step.get("line").and_then(Value::as_str) {
                ("command_line", json!({ "text": line }))
            } else if step.get("command").is_some_and(Value::is_string) {
                ("execute", step.clone())
            } else {
                ("step", step.clone())
            }
        })
        .collect()
}

/// `v` with every string `"$N"` replaced by the handle step N made (`made[N-1]`).
fn with_handles(v: &Value, made: &[Option<String>]) -> Value {
    match v {
        Value::String(t) => t
            .strip_prefix('$')
            .and_then(|n| n.parse::<usize>().ok())
            .and_then(|n| made.get(n.checked_sub(1)?).cloned().flatten())
            .map_or_else(|| v.clone(), Value::String),
        Value::Array(a) => Value::Array(a.iter().map(|x| with_handles(x, made)).collect()),
        Value::Object(o) => Value::Object(o.iter().map(|(k, x)| (k.clone(), with_handles(x, made))).collect()),
        other => other.clone(),
    }
}

/// The handle a step's result names: `handle`, or the first of `handles`.
fn made_handle(result: &str) -> Option<String> {
    let v: Value = serde_json::from_str(result).ok()?;
    let h = v.get("handle").or_else(|| v.get("handles").and_then(|h| h.get(0)))?;
    h.as_str().map(str::to_string)
}

fn cut(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_string() } else { format!("{}…", s.chars().take(n).collect::<String>()) }
}

/// Run a `run_commands` call: its steps in order, stopping at the first that fails.
fn run_steps(s: &mut Session, args: &Value) -> Outcome {
    let all = steps(args);
    if all.is_empty() {
        return Outcome::err("no steps".into(), "Ran nothing".into());
    }
    if all.len() > MAX_STEPS {
        return Outcome::err(format!("{} steps; send at most {MAX_STEPS} in one call", all.len()), "Ran nothing".into());
    }
    let total = all.len();
    let mut made: Vec<Option<String>> = Vec::new();
    let mut results = Vec::new();
    let mut names: Vec<String> = Vec::new();
    for (i, (name, a)) in all.into_iter().enumerate() {
        let out = if name == "step" {
            Outcome::err("a step needs \"command\" (with \"params\") or \"line\"".into(), "Skipped a step".into())
        } else {
            run(s, name, &with_handles(&a, &made))
        };
        let word = if name == "execute" {
            str_arg(&a, "command").trim().to_ascii_uppercase()
        } else {
            str_arg(&a, "text").split_whitespace().next().unwrap_or("").to_ascii_uppercase()
        };
        if !word.is_empty() && !word.starts_with(|c: char| c.is_ascii_digit() || c == '@' || c == '-') && !names.contains(&word) {
            names.push(word);
        }
        made.push(if out.ok { made_handle(&out.result) } else { None });
        results.push(json!({"step": i + 1, "ok": out.ok, "result": cut(&out.result, STEP_RESULT_CHARS)}));
        if !out.ok {
            let shown = names.iter().take(6).cloned().collect::<Vec<_>>().join(", ");
            return Outcome {
                result: compact(&json!({"done": i, "of": total, "stopped": i + 1, "results": results})),
                ok: false,
                line: format!("Ran {i} of {total} steps ({shown}); step {}: {}", i + 1, out.result.trim_start_matches("error: ")),
                picture: None,
            };
        }
    }
    let shown = names.iter().take(6).cloned().collect::<Vec<_>>().join(", ");
    let more = if names.len() > 6 { ", …" } else { "" };
    Outcome::ok(
        json!({"done": total, "of": total, "results": results}),
        format!("Ran {total} step{} ({shown}{more})", if total == 1 { "" } else { "s" }),
    )
}

/// What one call did: the text sent back to the model, and a short line for the chat panel.
pub struct Outcome {
    pub result: String,
    pub ok: bool,
    pub line: String,
    /// A picture for the model (a `data:image/png;base64,` URL), from `render`.
    pub picture: Option<String>,
}

impl Outcome {
    pub fn ok(v: Value, line: String) -> Outcome {
        Outcome { result: compact(&v), ok: true, line, picture: None }
    }
    pub fn err(e: String, line: String) -> Outcome {
        Outcome { result: format!("error: {e}"), ok: false, line: format!("{line}: {e}"), picture: None }
    }
}

/// Picture size for `render`, in pixels.
const PICTURE: (u32, u32) = (1280, 860);

/// A PNG of the drawing (or `window` of it) as a data URL.
fn render(s: &Session, args: &Value) -> std::result::Result<(Value, String), String> {
    let st = s.state().map_err(|e| e.to_string())?;
    let window = args.get("window").and_then(Value::as_array).and_then(|w| {
        let p = |i: usize| w.get(i)?.as_array().and_then(|a| Some(cadcraft_engine::geom::Vec2::new(a.first()?.as_f64()?, a.get(1)?.as_f64()?)));
        let (a, b) = (p(0)?, p(1)?);
        (a.x.is_finite() && a.y.is_finite() && b.x.is_finite() && b.y.is_finite() && a.x != b.x && a.y != b.y)
            .then(|| cadcraft_engine::geom::Bounds2 { min: a.min(b), max: a.max(b) })
    });
    let opts = cadcraft_render::Options { view: window, transparency: s.settings.transparency_display, ..Default::default() };
    let list = cadcraft_render::build(&st.doc, &st.space, &opts);
    let view = match &window {
        Some(w) => cadcraft_render::raster::View::of_window(w, Some(PICTURE.0), Some(PICTURE.1)),
        None => cadcraft_render::raster::View::fit(&list.bounds, PICTURE.0, PICTURE.1, 0.05),
    };
    let png = cadcraft_render::raster::render_png(&list, &view, &cadcraft_render::raster::RasterOptions::default())
        .ok_or("the picture could not be made")?;
    let url = format!("data:image/png;base64,{}", cadcraft_engine::cmd::file::base64_encode(&png));
    let shown = window.map_or_else(|| json!("the whole drawing"), |w| json!([[w.min.x, w.min.y], [w.max.x, w.max.y]]));
    Ok((json!({"picture": "attached below", "shows": shown, "width": view.width, "height": view.height}), url))
}

fn compact(v: &Value) -> String {
    let s = serde_json::to_string(v).unwrap_or_default();
    if s.chars().count() <= MAX_RESULT_CHARS {
        return s;
    }
    let cut: String = s.chars().take(MAX_RESULT_CHARS).collect();
    format!("{cut}… [cut at {MAX_RESULT_CHARS} characters; ask for less, e.g. with limit, layer or window]")
}

fn paged(args: &Value) -> Value {
    let mut a = args.as_object().cloned().unwrap_or_default();
    a.entry("limit").or_insert(json!(PAGE));
    Value::Object(a)
}

fn str_arg<'a>(args: &'a Value, k: &str) -> &'a str {
    args.get(k).and_then(Value::as_str).unwrap_or("")
}

/// The commands whose id, label, aliases or menu path contain the query's words, best first.
pub fn find_commands(s: &Session, query: &str) -> Value {
    let words: Vec<String> = query.split_whitespace().map(str::to_ascii_lowercase).collect();
    let mut found: Vec<(usize, Value)> = Vec::new();
    for spec in cadcraft_engine::command_specs() {
        let info = spec.info(s);
        let id = info.id.to_ascii_lowercase();
        let hay = format!(
            "{} {} {} {}",
            id,
            info.label.to_ascii_lowercase(),
            info.aliases.join(" ").to_ascii_lowercase(),
            info.menu.join(" ").to_ascii_lowercase()
        );
        let hits = words.iter().filter(|w| hay.contains(w.as_str())).count();
        if hits == 0 {
            continue;
        }
        // Whole matches on the id or an alias first.
        let exact = words.iter().any(|w| *w == id || info.aliases.iter().any(|a| a.eq_ignore_ascii_case(w)));
        let score = hits * 2 + usize::from(exact) * 10;
        found.push((score, json!({"id": info.id, "label": info.label, "aliases": info.aliases, "params": info.params, "enabled": info.enabled})));
    }
    found.sort_by_key(|f| std::cmp::Reverse(f.0));
    Value::Array(found.into_iter().take(MAX_FOUND).map(|(_, v)| v).collect())
}

fn app_state(s: &Session) -> Value {
    let selection = s.selection();
    json!({
        "running": s.running.as_ref().map(|r| r.id.clone()),
        "prompt": s.prompt_text(),
        "selectionCount": selection.len(),
        "selection": selection.iter().take(200).map(|h| h.hex()).collect::<Vec<_>>(),
        "history": s.log.iter().rev().take(10).rev().cloned().collect::<Vec<_>>(),
    })
}

/// Run tool `name` with `args` on the session.
pub fn run(s: &mut Session, name: &str, args: &Value) -> Outcome {
    let done = |v: std::result::Result<Value, String>, line: String| match v {
        Ok(v) => Outcome::ok(v, line),
        Err(e) => Outcome::err(e, line),
    };
    let ex = |s: &mut Session, id: &str, p: &Value| s.execute(id, p).map_err(|e| e.to_string());
    match name {
        "drawing_summary" => done(ex(s, "drawing.inspect", &json!({"entities": false})), "Read the drawing".into()),
        "query_entities" => done(ex(s, "entities", &paged(args)), "Looked at objects".into()),
        "find_text" => done(ex(s, "texts", &paged(args)), "Read the texts".into()),
        "find_commands" => done(Ok(find_commands(s, str_arg(args, "query"))), format!("Looked up \"{}\"", str_arg(args, "query"))),
        "app_state" => done(Ok(app_state(s)), "Checked what you're doing".into()),
        "load_skill" => {
            let name = str_arg(args, "name");
            match crate::skills::find(name) {
                Some(skill) => Outcome { result: skill.body, ok: true, line: format!("Read the skill \"{}\"", skill.name), picture: None },
                None => {
                    let names: Vec<String> = crate::skills::all().into_iter().map(|s| s.name).collect();
                    Outcome::err(format!("no skill `{name}`; there are: {}", names.join(", ")), "Looked for a skill".into())
                }
            }
        }
        "render" => match render(s, args) {
            Ok((v, url)) => Outcome { picture: Some(url), ..Outcome::ok(v, "Looked at the drawing".into()) },
            Err(e) => Outcome::err(e, "Could not make a picture".into()),
        },
        "run_lisp" => done(ex(s, "lisp", &json!({"code": str_arg(args, "code")})), "Ran a LISP routine".into()),
        "execute" => {
            let id = str_arg(args, "command").trim().to_ascii_lowercase();
            let params = args.get("params").filter(|p| p.is_object()).cloned().unwrap_or(json!({}));
            if id.is_empty() {
                return done(Err("missing command".into()), "Ran nothing".into());
            }
            done(ex(s, &id, &params), format!("Ran {}", id.to_ascii_uppercase()))
        }
        "run_commands" => run_steps(s, args),
        "command_line" => {
            let text = str_arg(args, "text").to_string();
            let before = s.log.len();
            let r = s.cmdline(&text).map_err(|e| e.to_string());
            let output: Vec<String> = s.log.iter().skip(before).cloned().collect();
            let line = format!("Typed `{text}`");
            match r {
                Ok(()) => done(Ok(json!({"prompt": s.prompt_text(), "output": output})), line),
                Err(e) => {
                    // A refused line leaves its reason in the history, like the user's.
                    s.echo(e.clone());
                    done(Err(e), line)
                }
            }
        }
        other => done(Err(format!("there is no tool `{other}`")), format!("Unknown tool {other}")),
    }
}
