//! `/ai`: the server side of CADCraft's AI assistant. The app sends a chat (messages and tool
//! definitions in the OpenAI chat form); the server adds the model and its key and passes it to
//! the provider, so no key is ever on a user's machine. The HTTPS call is made by `curl`, which
//! reads the key from its environment (never from its command line, which other users can see).

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Value, json};

/// The path the assistant connects to.
pub const AI: &str = "/ai";
/// OpenCode Go's OpenAI-compatible chat endpoint.
pub const DEFAULT_ENDPOINT: &str = "https://opencode.ai/inference/go/openai/v1/chat/completions";
pub const DEFAULT_MODEL: &str = "muse-spark-1.3-contributor";
/// The environment variable holding the provider's key (for the server and for `curl`).
pub const KEY_VAR: &str = "CADCRAFT_AI_KEY";
/// Most tokens one answer may have.
pub const MAX_TOKENS: u64 = 8000;
/// Most requests to the provider at once, over all users.
pub const MAX_AT_ONCE: usize = 8;
/// How long one request may take.
const WAIT_SECONDS: u32 = 180;
/// Most bytes of an answer read back.
const MAX_REPLY_BYTES: u64 = 4 << 20;

/// Where the server sends AI requests.
#[derive(Clone)]
pub struct AiConfig {
    pub endpoint: String,
    pub model: String,
    pub key: String,
}

/// Never shows the key.
impl std::fmt::Debug for AiConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AiConfig").field("endpoint", &self.endpoint).field("model", &self.model).finish_non_exhaustive()
    }
}

static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

/// One slot of [`MAX_AT_ONCE`], given back when dropped.
struct Slot;

impl Slot {
    fn take() -> Option<Slot> {
        IN_FLIGHT.try_update(Ordering::SeqCst, Ordering::SeqCst, |n| (n < MAX_AT_ONCE).then_some(n + 1)).ok().map(|_| Slot)
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The request the provider gets: the app's `messages` and `tools`, with the server's model and
/// token limit. Anything else the app sends (another model, say) is left out.
pub fn body(cfg: &AiConfig, request: &Value) -> Result<Value, String> {
    let messages = request.get("messages").and_then(Value::as_array).filter(|m| !m.is_empty()).ok_or("the request has no messages")?;
    let mut body = json!({"model": cfg.model, "messages": messages, "max_tokens": MAX_TOKENS});
    if let Some(tools) = request.get("tools").and_then(Value::as_array).filter(|t| !t.is_empty()) {
        body["tools"] = json!(tools);
    }
    Ok(body)
}

/// A conversation id fit for a header: letters, digits, `-` and `_`, at most 64.
fn header_safe(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').take(64).collect()
}

/// Ask the model; the reply is the provider's chat completion.
pub fn ask(cfg: &AiConfig, conversation: &str, request: &Value) -> Result<Value, String> {
    let body = body(cfg, request)?;
    let Some(_slot) = Slot::take() else { return Err("the AI is busy; try again in a moment".into()) };
    let started = std::time::Instant::now();
    let mut child = Command::new("curl")
        .args(["-sS", "--max-time", &WAIT_SECONDS.to_string(), "-X", "POST", &cfg.endpoint])
        .args(["-H", "Content-Type: application/json"])
        .args(["-H", &format!("User-Agent: cadcraft-ai/{}", env!("CARGO_PKG_VERSION"))])
        .args(["-H", &format!("x-opencode-session: {}", header_safe(conversation))])
        .args(["--variable", &format!("%{KEY_VAR}"), "--expand-header", &format!("Authorization: Bearer {{{{{KEY_VAR}}}}}")])
        .args(["--data-binary", "@-", "-w", "\n%{http_code}"])
        .env(KEY_VAR, &cfg.key)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("can't run curl: {e}"))?;
    let bytes = serde_json::to_vec(&body).map_err(|e| e.to_string())?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(&bytes).map_err(|e| format!("can't send the request: {e}"))?;
    }
    let mut out = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        stdout.take(MAX_REPLY_BYTES).read_to_end(&mut out).map_err(|e| e.to_string())?;
    }
    let mut err = String::new();
    if let Some(stderr) = child.stderr.take() {
        let _ = stderr.take(4096).read_to_string(&mut err);
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    if !status.success() {
        return Err(format!("the AI could not be reached: {}", err.trim()));
    }
    let text = String::from_utf8_lossy(&out);
    let (reply, code) = text.rsplit_once('\n').unwrap_or((&text, ""));
    let reply: Value = serde_json::from_str(reply).map_err(|_| format!("the AI answered with HTTP {code} and no readable reply"))?;
    if code.trim() != "200" {
        let why = reply.pointer("/error/message").and_then(Value::as_str).unwrap_or("no reason given");
        return Err(format!("the AI refused the request (HTTP {}): {}", code.trim(), why.chars().take(300).collect::<String>()));
    }
    let tokens = reply.pointer("/usage/total_tokens").and_then(Value::as_u64).unwrap_or(0);
    eprintln!("ai: conversation {}: {tokens} tokens in {:.1}s", header_safe(conversation), started.elapsed().as_secs_f64());
    Ok(reply)
}
