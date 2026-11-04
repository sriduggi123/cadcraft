//! CADCraft's AI assistant. The user asks in the chat panel; the [`Assistant`] sends the chat to
//! the cloud server's `/ai` (which holds the model's key), runs the tools the model asks for on
//! the open drawing ([`tools`]), sends their results back, and repeats until the model answers.
//! Everything one request changes is one undo step ("AI: …").
//!
//! Some calls need the user: a question ([`Need::Ask`]), points or objects picked in the drawing
//! ([`Need::Points`], [`Need::Objects`]), or a yes before something hard to take back
//! ([`Need::Confirm`]). The request waits for [`Assistant::answer`] and then goes on.
//!
//! It never blocks: the answer arrives on the WebSocket link and [`Assistant::tick`], called
//! every frame, picks it up.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![forbid(unsafe_code)]

pub mod prompt;
pub mod tools;

use std::collections::VecDeque;
use std::sync::mpsc::Receiver;

use cadcraft_collab::message::{ClientMsg, ServerMsg};
use cadcraft_collab::{Json, Link};
use cadcraft_engine::Session;
use serde_json::{Value, json};
use web_time::{Duration, Instant};

/// Most model replies (rounds of tool calls) for one request.
pub const MAX_ROUNDS: usize = 30;
/// How long one reply may take before the request is given up.
pub const WAIT: Duration = Duration::from_secs(200);
/// Most characters of one request the user types.
pub const MAX_PROMPT: usize = 8000;
/// Most messages kept in the chat sent to the model (older ones are dropped, oldest first).
const MAX_MESSAGES: usize = 200;
/// The LISP variable the picked points are left in.
const PICKS: &str = "*cadcraft-ai-picks*";

/// One line of the chat panel.
#[derive(Clone, Debug, PartialEq)]
pub enum Entry {
    User(String),
    Assistant(String),
    /// A tool call: what it did, and whether it worked.
    Tool {
        line: String,
        ok: bool,
    },
    Error(String),
}

/// What the request waits for from the user.
#[derive(Clone, Debug, PartialEq)]
pub enum Need {
    /// A yes or no before a call that is hard to take back. `lisp`: the call runs a LISP routine,
    /// which may also be allowed for the rest of the chat.
    Confirm { what: String, lisp: bool },
    /// A question, with suggested answers (the user may also type one).
    Ask { question: String, options: Vec<String> },
    /// Points clicked in the drawing (the command line's point prompt, with object snaps).
    Points { prompt: String, count: usize },
    /// Objects selected in the drawing, then Done.
    Objects { prompt: String },
}

/// The user's reply to a [`Need`].
#[derive(Clone, Debug, PartialEq)]
pub enum Answer {
    Allow,
    /// Allow this and every later LISP routine in this chat.
    AllowLisp,
    Deny,
    Text(String),
    /// The objects are selected.
    Done,
    Cancel,
}

/// What to do with one tool call.
enum Next {
    Run,
    Wait(Need),
    /// Already answered (e.g. a pick that could not start).
    Done(tools::Outcome),
}

struct Call {
    id: String,
    name: String,
    args: Value,
}

enum State {
    Idle,
    Connecting { rx: Receiver<Result<Box<dyn Link>, String>> },
    Waiting { id: u64, since: Instant },
    User { call: Call, need: Need },
}

/// The chat with the model and its connection.
pub struct Assistant {
    /// The server address (`wss://host`), `/ai` is added.
    server: String,
    link: Option<Box<dyn Link>>,
    state: State,
    /// The chat in the OpenAI form, without the system message.
    messages: Vec<Value>,
    pub log: Vec<Entry>,
    conversation: String,
    next_id: u64,
    rounds: usize,
    /// The request's undo step: the undo stack's length when it began, and its label.
    undo: Option<(usize, String)>,
    /// The request being connected for, sent once the link is up.
    queued: bool,
    /// The model's tool calls not run yet, in order.
    calls: VecDeque<Call>,
    /// Pictures from `render` this round, sent after the tool results.
    pictures: Vec<String>,
    /// The user allowed LISP routines for this chat.
    lisp_ok: bool,
}

/// An id for one chat, for the provider's session header: the time and a counter (not secret).
fn conversation_id() -> String {
    static COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let t = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    format!("cadcraft-{t:x}-{n}")
}

fn connect_later(url: &str) -> Receiver<Result<Box<dyn Link>, String>> {
    let (tx, rx) = std::sync::mpsc::channel();
    let url = url.to_string();
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = std::thread::Builder::new().name("ai-connect".into()).spawn(move || {
            let _ = tx.send(cadcraft_collab_net::connect(&url).map_err(|e| e.to_string()));
        });
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = tx.send(cadcraft_collab_net::connect(&url).map_err(|e| e.to_string()));
    }
    rx
}

/// Text for a LISP string literal.
fn lisp_string(s: &str) -> String {
    let clean: String = s.chars().filter(|c| !c.is_control()).take(200).collect();
    format!("\"{}\"", clean.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The points in a printed LISP list of points, e.g. `((1.0 2.0 0.0) (3.5 4.0 0.0))`; `nil`
/// entries (a pick the user skipped) are left out.
pub fn parse_points(printed: &str) -> Vec<[f64; 3]> {
    let mut out = Vec::new();
    let inner = printed.trim().strip_prefix('(').and_then(|s| s.strip_suffix(')')).unwrap_or("");
    let mut rest = inner;
    while let Some(open) = rest.find('(') {
        let Some(close) = rest[open..].find(')') else { break };
        let nums: Vec<f64> = rest[open + 1..open + close].split_whitespace().filter_map(|n| n.parse().ok()).filter(|n: &f64| n.is_finite()).collect();
        if nums.len() >= 2 {
            out.push([nums[0], nums[1], nums.get(2).copied().unwrap_or(0.0)]);
        }
        rest = &rest[open + close + 1..];
    }
    out
}

impl Assistant {
    /// An assistant using the cloud server at `server` (`wss://host` or `ws://host:port`).
    pub fn new(server: &str) -> Assistant {
        Assistant {
            server: server.trim_end_matches('/').to_string(),
            link: None,
            state: State::Idle,
            messages: Vec::new(),
            log: Vec::new(),
            conversation: conversation_id(),
            next_id: 1,
            rounds: 0,
            undo: None,
            queued: false,
            calls: VecDeque::new(),
            pictures: Vec::new(),
            lisp_ok: false,
        }
    }

    /// Use `link` to reach the AI instead of connecting to the server (tests use an in-memory one).
    pub fn attach(&mut self, link: Box<dyn Link>) {
        self.link = Some(link);
    }

    /// Use another server from the next request on.
    pub fn set_server(&mut self, server: &str) {
        let server = server.trim_end_matches('/');
        if server != self.server {
            self.server = server.to_string();
            self.link = None;
        }
    }

    /// Whether a request is in progress (also while it waits for the user).
    pub fn busy(&self) -> bool {
        !matches!(self.state, State::Idle)
    }

    /// What the request waits for from the user, if anything.
    pub fn need(&self) -> Option<&Need> {
        match &self.state {
            State::User { need, .. } => Some(need),
            _ => None,
        }
    }

    /// Start over: a new chat (the drawing keeps its changes).
    pub fn clear(&mut self, s: &mut Session) {
        self.stop(s);
        self.messages.clear();
        self.log.clear();
        self.lisp_ok = false;
        self.conversation = conversation_id();
    }

    /// Ask the model. The drawing as it is now goes with the request.
    pub fn ask(&mut self, s: &mut Session, text: &str) {
        let text: String = text.trim().chars().take(MAX_PROMPT).collect();
        if text.is_empty() || self.busy() {
            return;
        }
        self.log.push(Entry::User(text.clone()));
        let now = prompt::drawing_now(s);
        self.messages.push(json!({"role": "user", "content": format!("{text}\n\n[The drawing now: {now}]")}));
        if self.messages.len() > MAX_MESSAGES {
            self.trim();
        }
        self.rounds = 0;
        // One undo step for everything this request changes.
        if let Ok(st) = s.state_mut() {
            st.begin_undo_group();
            let label: String = text.chars().take(40).collect();
            self.undo = Some((st.undo.len(), format!("AI: {label}")));
        }
        self.send(s);
    }

    /// Drop the oldest messages, from a user message on, so the chat stays within bounds and
    /// still starts with what the user asked (never with a tool result).
    fn trim(&mut self) {
        let over = self.messages.len().saturating_sub(MAX_MESSAGES);
        let from = self.messages.iter().enumerate().skip(over.max(1)).find(|(_, m)| m["role"] == "user" && m["content"].is_string()).map(|(i, _)| i);
        if let Some(i) = from {
            self.messages.drain(..i);
        }
    }

    /// Stop the request: no more tool calls; what was changed stays, as one undo step.
    pub fn stop(&mut self, s: &mut Session) {
        if self.busy() {
            self.log.push(Entry::Error("Stopped.".into()));
            // A reply to an abandoned request must not be taken for the next one's.
            self.link = None;
        }
        if matches!(self.state, State::User { need: Need::Points { .. }, .. }) && s.lisp_running() {
            s.lisp_abort("Function cancelled");
        }
        self.state = State::Idle;
        self.queued = false;
        self.calls.clear();
        self.pictures.clear();
        self.finish(s);
    }

    fn finish(&mut self, s: &mut Session) {
        let Some((start, label)) = self.undo.take() else { return };
        if let Ok(st) = s.state_mut() {
            // The group's steps are now one, at the place the group began.
            st.end_undo_group();
            if let Some(step) = st.undo.get_mut(start) {
                step.label = label;
            }
        }
    }

    fn fail(&mut self, s: &mut Session, why: String) {
        self.log.push(Entry::Error(why));
        self.state = State::Idle;
        self.queued = false;
        self.calls.clear();
        self.pictures.clear();
        self.finish(s);
    }

    /// The chat as sent: only the latest picture is kept, older ones become a note (each costs
    /// thousands of tokens on every request).
    fn outgoing(&self) -> Vec<Value> {
        let last_picture = self.messages.iter().rposition(|m| m["content"].is_array());
        let mut out = vec![json!({"role": "system", "content": prompt::SYSTEM})];
        for (i, m) in self.messages.iter().enumerate() {
            if m["content"].is_array() && Some(i) != last_picture {
                out.push(json!({"role": "user", "content": "[an earlier picture of the drawing, no longer shown]"}));
            } else {
                out.push(m.clone());
            }
        }
        out
    }

    /// Send the chat as it is now.
    fn send(&mut self, s: &mut Session) {
        let messages = self.outgoing();
        let Some(link) = self.link.as_mut().filter(|l| l.is_open()) else {
            self.link = None;
            self.queued = true;
            self.state = State::Connecting { rx: connect_later(&format!("{}/ai", self.server)) };
            return;
        };
        let id = self.next_id;
        self.next_id += 1;
        let request = Json(json!({"messages": messages, "tools": tools::definitions()}));
        match link.send(&ClientMsg::Ai { id, conversation: self.conversation.clone(), request }) {
            Ok(()) => self.state = State::Waiting { id, since: Instant::now() },
            Err(e) => self.fail(s, format!("Could not reach the AI: {e}")),
        }
    }

    /// Pick up what arrived; run tool calls and send their results. Whether anything changed
    /// (the panel then repaints).
    pub fn tick(&mut self, s: &mut Session) -> bool {
        match &self.state {
            State::Idle => false,
            State::User { need: Need::Points { .. }, .. } => {
                if s.lisp_running() {
                    return false;
                }
                let State::User { call, .. } = std::mem::replace(&mut self.state, State::Idle) else { return false };
                let printed =
                    s.execute("lisp", &json!({"code": PICKS})).ok().and_then(|v| v.get("value").and_then(Value::as_str).map(str::to_string));
                let points = printed.as_deref().map(parse_points).unwrap_or_default();
                let out = if points.is_empty() {
                    tools::Outcome::err("the user did not pick a point".into(), "No points picked".into())
                } else {
                    tools::Outcome::ok(
                        json!({ "points": points }),
                        format!("You picked {} point{}", points.len(), if points.len() == 1 { "" } else { "s" }),
                    )
                };
                self.result(&call, out);
                self.work(s);
                true
            }
            State::User { .. } => false,
            State::Connecting { rx } => match rx.try_recv() {
                Ok(Ok(link)) => {
                    self.link = Some(link);
                    self.state = State::Idle;
                    if std::mem::take(&mut self.queued) {
                        self.send(s);
                    }
                    true
                }
                Ok(Err(e)) => {
                    self.fail(s, format!("Could not reach the AI at {}: {e}", self.server));
                    true
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => false,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.fail(s, "Could not reach the AI.".into());
                    true
                }
            },
            State::Waiting { id, since } => {
                let (id, since) = (*id, *since);
                let Some(link) = self.link.as_mut() else {
                    self.fail(s, "The connection to the AI closed.".into());
                    return true;
                };
                match link.recv() {
                    Some(Ok(ServerMsg::Ai { id: got, reply })) if got == id => {
                        match reply {
                            Ok(reply) => self.on_reply(s, reply.0),
                            Err(e) => self.fail(s, e),
                        }
                        true
                    }
                    Some(Ok(ServerMsg::Error { message })) => {
                        self.link = None;
                        self.fail(s, message);
                        true
                    }
                    Some(Ok(_)) => false,
                    Some(Err(e)) => {
                        self.link = None;
                        self.fail(s, format!("The connection to the AI broke: {e}"));
                        true
                    }
                    None if !link.is_open() => {
                        self.link = None;
                        self.fail(s, "The connection to the AI closed.".into());
                        true
                    }
                    None if since.elapsed() > WAIT => {
                        self.link = None;
                        self.fail(s, "The AI took too long to answer.".into());
                        true
                    }
                    None => false,
                }
            }
        }
    }

    /// The model answered: show its text; run its tool calls and go on, or end the request.
    fn on_reply(&mut self, s: &mut Session, reply: Value) {
        let Some(msg) = reply.pointer("/choices/0/message") else {
            return self.fail(s, "The AI sent an empty answer.".into());
        };
        let text = msg.get("content").and_then(Value::as_str).unwrap_or("").trim().to_string();
        let calls: Vec<Value> = msg.get("tool_calls").and_then(Value::as_array).cloned().unwrap_or_default();
        let mut kept = json!({"role": "assistant", "content": text});
        if !calls.is_empty() {
            kept["tool_calls"] = json!(calls);
        }
        self.messages.push(kept);
        if !text.is_empty() {
            self.log.push(Entry::Assistant(text));
        }
        if calls.is_empty() {
            self.state = State::Idle;
            self.finish(s);
            return;
        }
        self.state = State::Idle;
        self.calls = calls
            .iter()
            .map(|c| Call {
                id: c.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
                name: c.pointer("/function/name").and_then(Value::as_str).unwrap_or("").to_string(),
                args: {
                    let raw = c.pointer("/function/arguments").and_then(Value::as_str).unwrap_or("{}");
                    serde_json::from_str(if raw.trim().is_empty() { "{}" } else { raw }).unwrap_or(Value::Null)
                },
            })
            .collect();
        self.work(s);
    }

    /// Record a call's result for the model and the panel.
    fn result(&mut self, call: &Call, out: tools::Outcome) {
        self.log.push(Entry::Tool { line: out.line, ok: out.ok });
        self.messages.push(json!({"role": "tool", "tool_call_id": call.id, "content": out.result}));
        if let Some(p) = out.picture {
            self.pictures.push(p);
        }
    }

    /// Run the waiting calls in order until one needs the user; when all are done, send.
    fn work(&mut self, s: &mut Session) {
        while let Some(call) = self.calls.pop_front() {
            if !call.args.is_object() {
                let line = format!("Could not read the call to {}", call.name);
                self.result(&call, tools::Outcome::err("the arguments are not a JSON object".into(), line));
                continue;
            }
            match self.need_for(s, &call) {
                Next::Wait(need) => {
                    self.state = State::User { call, need };
                    return;
                }
                Next::Done(out) => self.result(&call, out),
                Next::Run => {
                    let out = tools::run(s, &call.name, &call.args);
                    self.result(&call, out);
                }
            }
        }
        if !self.pictures.is_empty() {
            let mut content = vec![json!({"type": "text", "text": "The picture render made:"})];
            content.extend(self.pictures.drain(..).map(|url| json!({"type": "image_url", "image_url": {"url": url}})));
            self.messages.push(json!({"role": "user", "content": content}));
        }
        self.rounds += 1;
        if self.rounds >= MAX_ROUNDS {
            return self.fail(s, format!("Stopped after {MAX_ROUNDS} steps; ask again to go on."));
        }
        self.send(s);
    }

    /// What to do with `call`: run it, or wait for the user. Starting a point pick happens here:
    /// the command line asks for the points.
    fn need_for(&mut self, s: &mut Session, call: &Call) -> Next {
        let text = |k: &str| call.args.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string();
        match call.name.as_str() {
            "ask_user" => {
                let options = call
                    .args
                    .get("options")
                    .and_then(Value::as_array)
                    .map(|o| o.iter().filter_map(Value::as_str).take(8).map(str::to_string).collect())
                    .unwrap_or_default();
                Next::Wait(Need::Ask { question: text("question"), options })
            }
            "pick_objects" => Next::Wait(Need::Objects { prompt: text("prompt") }),
            "pick_points" => {
                let count = call.args.get("count").and_then(Value::as_u64).unwrap_or(1).clamp(1, 20) as usize;
                let prompt = if text("prompt").is_empty() { "Pick a point".to_string() } else { text("prompt") };
                let picks: Vec<String> = (1..=count)
                    .map(|i| {
                        format!(
                            "(getpoint {})",
                            lisp_string(&if count == 1 { format!("{prompt}: ") } else { format!("{prompt} ({i} of {count}): ") })
                        )
                    })
                    .collect();
                let code = format!("(setq {PICKS} nil) (setq {PICKS} (list {}))", picks.join(" "));
                match s.lisp_eval(&code, false, false, Vec::new()) {
                    Ok(_) if s.lisp_running() => Next::Wait(Need::Points { prompt, count }),
                    Ok(_) => Next::Done(tools::Outcome::err("no point prompt could be shown".into(), "Could not ask for points".into())),
                    Err(e) => Next::Done(tools::Outcome::err(e.to_string(), "Could not ask for points".into())),
                }
            }
            _ => match tools::permission(s, &call.name, &call.args, self.lisp_ok) {
                Some(what) => Next::Wait(Need::Confirm { lisp: call.name == "run_lisp", what }),
                None => Next::Run,
            },
        }
    }

    /// The user's reply to what the request waits for ([`Assistant::need`]).
    pub fn answer(&mut self, s: &mut Session, answer: Answer) {
        let State::User { need, .. } = &self.state else { return };
        // A point pick ends when its LISP routine does; cancelling it cancels the routine.
        if matches!(need, Need::Points { .. }) {
            if answer == Answer::Cancel && s.lisp_running() {
                s.lisp_abort("Function cancelled");
            }
            return;
        }
        let State::User { call, need } = std::mem::replace(&mut self.state, State::Idle) else { return };
        let out = match (need, answer) {
            (Need::Confirm { .. }, Answer::Allow) => tools::run(s, &call.name, &call.args),
            (Need::Confirm { .. }, Answer::AllowLisp) => {
                self.lisp_ok = true;
                tools::run(s, &call.name, &call.args)
            }
            (Need::Confirm { what, .. }, _) => {
                tools::Outcome::err("the user said no; don't do it another way".into(), format!("You declined: {what}"))
            }
            (Need::Ask { .. }, Answer::Text(t)) if !t.trim().is_empty() => {
                let t: String = t.trim().chars().take(MAX_PROMPT).collect();
                tools::Outcome::ok(json!({ "answer": t }), format!("You answered: {t}"))
            }
            (Need::Ask { .. }, _) => tools::Outcome::err("the user did not answer".into(), "No answer".into()),
            (Need::Objects { .. }, Answer::Done) => {
                let handles: Vec<String> = s.selection().iter().take(5000).map(|h| h.hex()).collect();
                let line = format!("You selected {} object{}", handles.len(), if handles.len() == 1 { "" } else { "s" });
                if handles.is_empty() {
                    tools::Outcome::err("the user selected nothing".into(), line)
                } else {
                    tools::Outcome::ok(json!({ "handles": handles }), line)
                }
            }
            (Need::Objects { .. }, _) => tools::Outcome::err("the user did not select objects".into(), "No objects selected".into()),
            (Need::Points { .. }, _) => return,
        };
        self.result(&call, out);
        self.work(s);
    }
}

#[cfg(test)]
mod tests;
