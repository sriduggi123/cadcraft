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
    ])
}

/// What one call did: the text sent back to the model, and a short line for the chat panel.
pub struct Outcome {
    pub result: String,
    pub ok: bool,
    pub line: String,
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
        Ok(v) => Outcome { result: compact(&v), ok: true, line },
        Err(e) => Outcome { result: format!("error: {e}"), ok: false, line: format!("{line}: {e}") },
    };
    let ex = |s: &mut Session, id: &str, p: &Value| s.execute(id, p).map_err(|e| e.to_string());
    match name {
        "drawing_summary" => done(ex(s, "drawing.inspect", &json!({"entities": false})), "Read the drawing".into()),
        "query_entities" => done(ex(s, "entities", &paged(args)), "Looked at objects".into()),
        "find_text" => done(ex(s, "texts", &paged(args)), "Read the texts".into()),
        "find_commands" => done(Ok(find_commands(s, str_arg(args, "query"))), format!("Looked up \"{}\"", str_arg(args, "query"))),
        "app_state" => done(Ok(app_state(s)), "Checked what you're doing".into()),
        "execute" => {
            let id = str_arg(args, "command").trim().to_ascii_lowercase();
            let params = args.get("params").filter(|p| p.is_object()).cloned().unwrap_or(json!({}));
            if id.is_empty() {
                return done(Err("missing command".into()), "Ran nothing".into());
            }
            done(ex(s, &id, &params), format!("Ran {}", id.to_ascii_uppercase()))
        }
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
