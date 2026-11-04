//! CADCraft's AI assistant. The user asks in the chat panel; the [`Assistant`] sends the chat to
//! the cloud server's `/ai` (which holds the model's key), runs the tools the model asks for on
//! the open drawing ([`tools`]), sends their results back, and repeats until the model answers.
//! Everything one request changes is one undo step ("AI: …").
//!
//! It never blocks: the answer arrives on the WebSocket link and [`Assistant::tick`], called
//! every frame, picks it up.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![forbid(unsafe_code)]

pub mod prompt;
pub mod tools;

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

enum State {
    Idle,
    Connecting { rx: Receiver<Result<Box<dyn Link>, String>> },
    Waiting { id: u64, since: Instant },
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

    /// Whether a request is in progress.
    pub fn busy(&self) -> bool {
        !matches!(self.state, State::Idle)
    }

    /// Start over: a new chat (the drawing keeps its changes).
    pub fn clear(&mut self, s: &mut Session) {
        self.stop(s);
        self.messages.clear();
        self.log.clear();
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
        let from = self.messages.iter().enumerate().skip(over.max(1)).find(|(_, m)| m["role"] == "user").map(|(i, _)| i);
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
        self.state = State::Idle;
        self.queued = false;
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
        self.finish(s);
    }

    /// Send the chat as it is now.
    fn send(&mut self, s: &mut Session) {
        let Some(link) = self.link.as_mut().filter(|l| l.is_open()) else {
            self.link = None;
            self.queued = true;
            self.state = State::Connecting { rx: connect_later(&format!("{}/ai", self.server)) };
            return;
        };
        let id = self.next_id;
        self.next_id += 1;
        let mut messages = vec![json!({"role": "system", "content": prompt::SYSTEM})];
        messages.extend(self.messages.iter().cloned());
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
        for call in &calls {
            let id = call.get("id").and_then(Value::as_str).unwrap_or("").to_string();
            let name = call.pointer("/function/name").and_then(Value::as_str).unwrap_or("");
            let raw = call.pointer("/function/arguments").and_then(Value::as_str).unwrap_or("{}");
            let out = match serde_json::from_str::<Value>(if raw.trim().is_empty() { "{}" } else { raw }) {
                Ok(args) => tools::run(s, name, &args),
                Err(e) => tools::Outcome {
                    result: format!("error: the arguments are not JSON: {e}"),
                    ok: false,
                    line: format!("Could not read the call to {name}"),
                },
            };
            self.log.push(Entry::Tool { line: out.line, ok: out.ok });
            self.messages.push(json!({"role": "tool", "tool_call_id": id, "content": out.result}));
        }
        self.rounds += 1;
        if self.rounds >= MAX_ROUNDS {
            return self.fail(s, format!("Stopped after {MAX_ROUNDS} steps; ask again to go on."));
        }
        self.send(s);
    }
}

#[cfg(test)]
mod tests;
