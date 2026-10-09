//! Two ways to reach Claude, behind one trait:
//! - [`ApiClient`]: the Messages API with `ANTHROPIC_API_KEY`. Required for any
//!   hosted or shared use.
//! - [`ClaudeCodeClient`]: the local `claude` CLI in print mode, signed in with
//!   the developer's own account. For personal runs only — Anthropic does not
//!   allow routing other people's requests through a Free/Pro/Max login.

use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};
use std::thread::sleep;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use regex::Regex;
use serde_json::{json, Value};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub calls: u64,
}

impl Usage {
    pub fn add(&mut self, u: &Value) {
        let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
        self.input_tokens += n("input_tokens");
        self.output_tokens += n("output_tokens");
        self.cache_read_tokens += n("cache_read_input_tokens");
        self.cache_write_tokens += n("cache_creation_input_tokens");
        self.calls += 1;
    }

    pub fn to_json(&self) -> Value {
        json!({
            "input_tokens": self.input_tokens, "output_tokens": self.output_tokens,
            "cache_read_tokens": self.cache_read_tokens, "cache_write_tokens": self.cache_write_tokens,
            "calls": self.calls,
        })
    }
}

/// Anything that can turn (system, user) into Claude's reply text.
pub trait Llm: Send + Sync {
    fn complete(&self, model: &str, system: &str, user: &str, max_tokens: u32) -> Result<String>;
    fn usage(&self) -> Usage;
}

static FATAL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)usage limit|limit reached|hit your limit|credit balance|not logged in|/login|invalid api key|invalid x-api-key|authentication|permission denied|quota").unwrap()
});

/// Errors after which every further call would fail the same way
/// (usage limit, not signed in, bad key). Callers stop instead of retrying.
pub fn is_fatal(message: &str) -> bool {
    FATAL.is_match(message)
}

// ---------------------------------------------------------------------------

const ENDPOINT: &str = "https://api.anthropic.com/v1/messages";
const API_VERSION: &str = "2023-06-01";
const RETRYABLE: &[u16] = &[408, 429, 500, 502, 503, 504, 529];

/// Minimal Messages API client. Retries rate limits, overloads, server and
/// network errors with exponential backoff, honouring `retry-after`.
pub struct ApiClient {
    agent: ureq::Agent,
    api_key: String,
    max_retries: u32,
    usage: Mutex<Usage>,
}

impl ApiClient {
    pub fn from_env() -> Result<Self> {
        let key = std::env::var("ANTHROPIC_API_KEY").unwrap_or_default();
        if key.is_empty() {
            bail!("Set ANTHROPIC_API_KEY (create one in the Claude Console), or use --backend claude-code.");
        }
        Ok(Self {
            agent: ureq::AgentBuilder::new()
                .timeout_connect(Duration::from_secs(20))
                .timeout_read(Duration::from_secs(300))
                .build(),
            api_key: key,
            max_retries: 5,
            usage: Mutex::new(Usage::default()),
        })
    }
}

impl Llm for ApiClient {
    fn complete(&self, model: &str, system: &str, user: &str, max_tokens: u32) -> Result<String> {
        // The system prompt is identical across every module call in a run, so
        // marking it cacheable lets repeated calls read it from the prompt cache.
        let body = json!({
            "model": model,
            "max_tokens": max_tokens,
            "system": [{ "type": "text", "text": system, "cache_control": { "type": "ephemeral" } }],
            "messages": [{ "role": "user", "content": user }],
        });

        let mut attempt = 0;
        let data: Value = loop {
            attempt += 1;
            let res = self
                .agent
                .post(ENDPOINT)
                .set("x-api-key", &self.api_key)
                .set("anthropic-version", API_VERSION)
                .set("content-type", "application/json")
                .send_json(body.clone());
            match res {
                Ok(resp) => break resp.into_json().context("Claude API returned invalid JSON")?,
                Err(ureq::Error::Status(code, resp)) if RETRYABLE.contains(&code) && attempt <= self.max_retries => {
                    let wait = resp
                        .header("retry-after")
                        .and_then(|v| v.parse::<f64>().ok())
                        .filter(|v| *v > 0.0)
                        .map(|v| v.min(60.0))
                        .unwrap_or_else(|| backoff(attempt));
                    sleep(Duration::from_secs_f64(wait));
                }
                Err(ureq::Error::Status(code, resp)) => {
                    let text = resp.into_string().unwrap_or_default();
                    let msg = serde_json::from_str::<Value>(&text)
                        .ok()
                        .and_then(|v| v.pointer("/error/message").and_then(Value::as_str).map(String::from))
                        .unwrap_or_else(|| text.chars().take(300).collect());
                    bail!("Claude API error {code}: {msg}");
                }
                Err(ureq::Error::Transport(t)) => {
                    if attempt > self.max_retries {
                        bail!("Network error talking to the Claude API: {t}");
                    }
                    sleep(Duration::from_secs_f64(backoff(attempt)));
                }
            }
        };

        self.usage.lock().unwrap().add(data.get("usage").unwrap_or(&Value::Null));
        if data.get("stop_reason").and_then(Value::as_str) == Some("max_tokens") {
            eprintln!("owlmap: a reply was cut off at max_tokens ({model}); output may be incomplete.");
        }
        Ok(text_blocks(&data))
    }

    fn usage(&self) -> Usage {
        *self.usage.lock().unwrap()
    }
}

fn text_blocks(data: &Value) -> String {
    data.get("content")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect::<String>()
        })
        .unwrap_or_default()
}

fn backoff(attempt: u32) -> f64 {
    let jitter = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_millis())
        .unwrap_or(0) as f64)
        / 1000.0;
    (2f64.powi(attempt as i32) + jitter).min(60.0)
}

// ---------------------------------------------------------------------------

/// Runs prompts through `claude -p` with no tools, no MCP servers and no saved
/// session, in an empty temporary folder, so Claude sees only what we send.
pub struct ClaudeCodeClient {
    bin: String,
    retries: u32,
    safe_mode: AtomicBool,
    usage: Mutex<Usage>,
}

impl ClaudeCodeClient {
    pub fn new(bin: Option<String>) -> Result<Self> {
        let bin = bin.or_else(|| std::env::var("OWLMAP_CLAUDE_BIN").ok()).unwrap_or_else(|| "claude".into());
        let ok = Command::new(&bin)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|_| anyhow!("Claude Code (`{bin}`) is not installed. See https://code.claude.com/docs/en/setup"))?;
        if !ok.success() {
            bail!("`{bin} --version` failed.");
        }
        Ok(Self { bin, retries: 2, safe_mode: AtomicBool::new(true), usage: Mutex::new(Usage::default()) })
    }

    /// Number of retries for transient failures (default 2).
    pub fn with_retries(mut self, n: u32) -> Self {
        self.retries = n;
        self
    }

    fn run(&self, dir: &std::path::Path, model: &str, prompt_file: &std::path::Path, user: &str) -> Result<std::process::Output> {
        let mut cmd = Command::new(&self.bin);
        cmd.args(["-p", "--output-format", "json", "--model", model, "--system-prompt-file"])
            .arg(prompt_file)
            .args(["--tools", "", "--strict-mcp-config", "--max-turns", "1", "--no-session-persistence"]);
        if self.safe_mode.load(Ordering::Relaxed) {
            cmd.arg("--safe-mode");
        }
        let mut child = cmd
            .current_dir(dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("could not start Claude Code")?;
        // Write the prompt on another thread so a large prompt can't deadlock
        // against a child that is already filling its stdout pipe.
        let mut stdin = child.stdin.take().unwrap();
        let input = user.to_string();
        let writer = std::thread::spawn(move || {
            let _ = stdin.write_all(input.as_bytes());
        });
        let out = child.wait_with_output()?;
        let _ = writer.join();
        Ok(out)
    }
}

impl Llm for ClaudeCodeClient {
    /// Retries transient failures twice (5 s, then 20 s); fatal ones fail at once.
    fn complete(&self, model: &str, system: &str, user: &str, _max_tokens: u32) -> Result<String> {
        let mut attempt = 0;
        loop {
            match self.complete_once(model, system, user) {
                Ok(r) => return Ok(r),
                Err(e) if attempt < self.retries && !is_fatal(&e.to_string()) => {
                    sleep(Duration::from_secs(if attempt == 0 { 5 } else { 20 }));
                    attempt += 1;
                }
                Err(e) => return Err(e),
            }
        }
    }

    fn usage(&self) -> Usage {
        *self.usage.lock().unwrap()
    }
}

impl ClaudeCodeClient {
    fn complete_once(&self, model: &str, system: &str, user: &str) -> Result<String> {
        // Empty working directory: no project CLAUDE.md, settings or files to pick up.
        let dir = tempfile::Builder::new().prefix("owlmap-cc-").tempdir()?;
        let prompt_file = dir.path().join("system.txt");
        std::fs::write(&prompt_file, system)?;

        let mut out = self.run(dir.path(), model, &prompt_file, user)?;
        let stderr = String::from_utf8_lossy(&out.stderr).to_lowercase();
        if !out.status.success() && self.safe_mode.load(Ordering::Relaxed) && stderr.contains("unknown option") && stderr.contains("safe-mode") {
            self.safe_mode.store(false, Ordering::Relaxed); // older Claude Code without --safe-mode
            out = self.run(dir.path(), model, &prompt_file, user)?;
        }

        let data: Option<Value> = serde_json::from_slice(&out.stdout).ok();
        let failed = !out.status.success() || data.as_ref().and_then(|d| d.get("is_error")).and_then(Value::as_bool).unwrap_or(false);
        match data {
            Some(d) if !failed => {
                self.usage.lock().unwrap().add(d.get("usage").unwrap_or(&Value::Null));
                Ok(d.get("result").and_then(Value::as_str).unwrap_or("").to_string())
            }
            other => {
                let detail = other
                    .as_ref()
                    .and_then(|d| d.get("result").and_then(Value::as_str).map(String::from))
                    .or_else(|| String::from_utf8_lossy(&out.stderr).lines().last().map(String::from))
                    .unwrap_or_else(|| format!("exit {:?}", out.status.code()));
                bail!("Claude Code call failed: {}", detail.trim().chars().take(300).collect::<String>())
            }
        }
    }
}
