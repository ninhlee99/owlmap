//! Runs the two-stage pipeline:
//! 1. Summarise each module in parallel with the fast model (structured JSON).
//! 2. Write ARCHITECTURE, FLOWS and ONBOARDING with the smart model, from the
//!    file tree, manifests and module summaries (never from raw code again).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Map, Value};

use crate::client::Llm;
use crate::config::Config;
use crate::grouper::{Grouper, Module};
use crate::prompts;
use crate::scanner::{ScanResult, Scanner, SourceFile};
use crate::estimate_tokens;

const MANIFEST_NAMES: &[&str] = &[
    "README.md", "README", "readme.md", "package.json", "Gemfile", "pyproject.toml", "requirements.txt",
    "go.mod", "Cargo.toml", "composer.json", "pom.xml", "build.gradle", "mix.exs", "pubspec.yaml",
    "Dockerfile", "docker-compose.yml", "compose.yaml", "Makefile", "Procfile", ".tool-versions",
    "config/routes.rb", "config/database.yml", "app.json", "vercel.json",
];
const MANIFEST_MAX_CHARS: usize = 8_000;
const MANIFESTS_TOTAL_MAX: usize = 40_000;
const TREE_MAX_LINES: usize = 1_500;
/// Used only for the up-front estimate.
const SUMMARY_TOKENS_PER_MODULE: u64 = 700;
const SUMMARY_KEYS: &[&str] = &["purpose", "key_files", "public_interface", "depends_on", "used_by", "data", "risks", "notes"];

#[derive(Debug)]
pub struct Plan {
    pub files: Vec<SourceFile>,
    pub skipped: BTreeMap<String, usize>,
    pub modules: Vec<Module>,
    pub estimated_input_tokens: u64,
}

pub struct RunResult {
    pub plan: Plan,
    /// One JSON object per module, in module order.
    pub summaries: Vec<Map<String, Value>>,
    /// File name → Markdown.
    pub documents: BTreeMap<String, String>,
    pub failures: Vec<String>,
}

pub struct Analyzer<'a> {
    root: PathBuf,
    repo_name: String,
    config: &'a Config,
    quiet: bool,
}

impl<'a> Analyzer<'a> {
    pub fn new(root: impl Into<PathBuf>, repo_name: &str, config: &'a Config) -> Self {
        Self { root: root.into(), repo_name: repo_name.into(), config, quiet: false }
    }

    pub fn quiet(mut self) -> Self {
        self.quiet = true;
        self
    }

    /// Everything that can be known without calling the API.
    pub fn plan(&self) -> Result<Plan> {
        let ScanResult { files, skipped } = Scanner::new(&self.root, self.config.max_file_bytes).scan();
        if files.is_empty() {
            bail!("No source files found to document.");
        }
        if files.len() > self.config.max_files {
            bail!(
                "{} source files found; the beta limit is {}. Use --max-files to raise it if you accept the cost.",
                files.len(),
                self.config.max_files
            );
        }
        let modules = Grouper::new(self.config.module_char_budget, self.config.min_module_chars).group(&files);
        let estimated_input_tokens = estimate(&files, &modules);
        Ok(Plan { files, skipped, modules, estimated_input_tokens })
    }

    pub fn run(&self, client: &dyn Llm) -> Result<RunResult> {
        let plan = self.plan()?;
        if plan.estimated_input_tokens > self.config.max_input_tokens {
            bail!(
                "Estimated {} input tokens exceeds the limit of {}. Raise --max-input-tokens to proceed.",
                plan.estimated_input_tokens,
                self.config.max_input_tokens
            );
        }

        self.say(&format!("Summarising {} modules with {}…", plan.modules.len(), self.config.fast_model));
        let (summaries, failures) = self.summarise_modules(&plan.modules, client);

        self.say(&format!("Writing architecture, flows and onboarding with {}…", self.config.smart_model));
        let documents = self.synthesise(&plan, &summaries, client);

        Ok(RunResult { plan, summaries, documents, failures })
    }

    fn summarise_modules(&self, modules: &[Module], client: &dyn Llm) -> (Vec<Map<String, Value>>, Vec<String>) {
        let next = AtomicUsize::new(0);
        let done = AtomicUsize::new(0);
        let results: Mutex<Vec<Option<Map<String, Value>>>> = Mutex::new(vec![None; modules.len()]);
        let workers = self.config.concurrency.max(1).min(modules.len().max(1));

        std::thread::scope(|s| {
            for _ in 0..workers {
                s.spawn(|| loop {
                    let i = next.fetch_add(1, Ordering::SeqCst);
                    let Some(m) = modules.get(i) else { break };
                    let summary = self.summarise(m, client);
                    let failed = summary.contains_key("error");
                    results.lock().unwrap()[i] = Some(summary);
                    let n = done.fetch_add(1, Ordering::SeqCst) + 1;
                    self.say(&format!("  [{n}/{}] {}{}", modules.len(), m.name, if failed { " (failed)" } else { "" }));
                });
            }
        });

        let summaries: Vec<_> = results.into_inner().unwrap().into_iter().map(Option::unwrap).collect();
        let failures = summaries
            .iter()
            .filter(|s| s.contains_key("error"))
            .map(|s| s["module"].as_str().unwrap_or_default().to_string())
            .collect();
        (summaries, failures)
    }

    fn summarise(&self, m: &Module, client: &dyn Llm) -> Map<String, Value> {
        let mut out = Map::new();
        out.insert("module".into(), json!(m.name));
        out.insert("slug".into(), json!(m.slug()));
        out.insert("files".into(), json!(m.files.iter().map(|f| f.path.clone()).collect::<Vec<_>>()));

        let text = m
            .files
            .iter()
            .map(|f| format!("=== {} ({}) ===\n{}", f.path, f.language, f.read(&self.root)))
            .collect::<Vec<_>>()
            .join("\n\n");
        let reply = match client.complete(&self.config.fast_model, &prompts::MODULE_SYSTEM, &prompts::module_user(&m.name, &text), 4_000) {
            Ok(r) => r,
            Err(e) => {
                out.insert("error".into(), json!(e.to_string()));
                return out;
            }
        };
        match parse_json_object(&reply) {
            Ok(Value::Object(obj)) => {
                for k in SUMMARY_KEYS {
                    if let Some(v) = obj.get(*k) {
                        out.insert((*k).into(), v.clone());
                    }
                }
            }
            _ => {
                out.insert("purpose".into(), json!(""));
                out.insert("notes".into(), json!(reply.trim()));
                out.insert("parse_error".into(), json!(true));
            }
        }
        out
    }

    fn synthesise(&self, plan: &Plan, summaries: &[Map<String, Value>], client: &dyn Llm) -> BTreeMap<String, String> {
        let usable: Vec<Value> = summaries
            .iter()
            .filter(|s| !s.contains_key("error"))
            .map(|s| {
                let mut s = s.clone();
                s.remove("files");
                Value::Object(s)
            })
            .collect();
        let modules_json = serde_json::to_string_pretty(&usable).unwrap_or_default();
        let tree = tree_text(&plan.files);
        let manifests = manifests_text(&self.root, &plan.files);
        let tasks = [
            ("ARCHITECTURE.md", prompts::ARCHITECTURE_TASK),
            ("FLOWS.md", prompts::FLOWS_TASK),
            ("ONBOARDING.md", prompts::ONBOARDING_TASK),
        ];

        std::thread::scope(|s| {
            let handles: Vec<_> = tasks
                .iter()
                .map(|(file, task)| {
                    let user = prompts::synthesis_user(&self.repo_name, &tree, &manifests, &modules_json, task);
                    s.spawn(move || {
                        let text = match client.complete(&self.config.smart_model, &prompts::SYNTHESIS_SYSTEM, &user, 8_000) {
                            Ok(t) => strip_outer_fence(&t),
                            Err(e) => format!(
                                "# {}\n\nOwlMap could not write this document: {e}\n",
                                file.trim_end_matches(".md").to_lowercase()
                            ),
                        };
                        (file.to_string(), text)
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        })
    }

    fn say(&self, msg: &str) {
        if !self.quiet {
            eprintln!("{msg}");
        }
    }
}

/// Extracts the first JSON object from a reply, tolerating code fences and chatter.
pub fn parse_json_object(text: &str) -> Result<Value> {
    let (Some(first), Some(last)) = (text.find('{'), text.rfind('}')) else {
        return Err(anyhow!("no JSON object in reply"));
    };
    if last <= first {
        return Err(anyhow!("no JSON object in reply"));
    }
    Ok(serde_json::from_str(&text[first..=last])?)
}

fn strip_outer_fence(text: &str) -> String {
    let t = text.trim();
    if let Some(rest) = t.strip_prefix("```") {
        if let Some(nl) = rest.find('\n') {
            let lang = rest[..nl].trim().to_lowercase();
            if lang.is_empty() || lang == "markdown" || lang == "md" {
                let body = &rest[nl + 1..];
                let body = body.trim_end().strip_suffix("```").unwrap_or(body);
                return format!("{}\n", body.trim_end());
            }
        }
    }
    format!("{t}\n")
}

fn tree_text(files: &[SourceFile]) -> String {
    let mut lines: Vec<&str> = files.iter().map(|f| f.path.as_str()).take(TREE_MAX_LINES).collect();
    let extra = files.len().saturating_sub(TREE_MAX_LINES);
    let tail;
    if extra > 0 {
        tail = format!("… {extra} more files");
        lines.push(&tail);
    }
    lines.join("\n")
}

fn manifests_text(root: &Path, files: &[SourceFile]) -> String {
    let mut total = 0;
    let mut parts = Vec::new();
    for f in files.iter().filter(|f| MANIFEST_NAMES.contains(&f.path.as_str())) {
        if total >= MANIFESTS_TOTAL_MAX {
            break;
        }
        let body: String = f.read(root).chars().take(MANIFEST_MAX_CHARS).collect();
        total += body.len();
        parts.push(format!("=== {} ===\n{body}", f.path));
    }
    if parts.is_empty() { "(none found)".into() } else { parts.join("\n\n") }
}

fn estimate(files: &[SourceFile], modules: &[Module]) -> u64 {
    let prompt_overhead = estimate_tokens(prompts::MODULE_SYSTEM.len() as u64) + 50;
    let module_tokens = estimate_tokens(files.iter().map(SourceFile::prompt_size).sum()) + modules.len() as u64 * prompt_overhead;
    let tree_tokens = estimate_tokens(files.iter().map(|f| f.path.len() as u64 + 1).sum());
    let synthesis_once = tree_tokens
        + estimate_tokens(MANIFESTS_TOTAL_MAX as u64) / 2
        + modules.len() as u64 * SUMMARY_TOKENS_PER_MODULE
        + estimate_tokens(prompts::SYNTHESIS_SYSTEM.len() as u64);
    module_tokens + synthesis_once * 3
}
