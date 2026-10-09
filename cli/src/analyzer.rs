//! Runs the two-stage pipeline:
//! 1. Summarise each module in parallel with the fast model (structured JSON).
//! 2. Write ARCHITECTURE, FLOWS and ONBOARDING with the smart model, from the
//!    file tree, manifests and module summaries (never from raw code again).
//!
//! Both stages consult the previous run's cache (see [`crate::cache`]): a call
//! whose exact input was seen before is answered from the cache, not Claude.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Map, Value};

use crate::cache;
use crate::client::Llm;
use crate::config::Config;
use crate::estimate_tokens;
use crate::grouper::{Grouper, Module};
use crate::prompts;
use crate::scanner::{ScanResult, Scanner, SourceFile};

const MANIFEST_NAMES: &[&str] = &[
    "README.md", "README", "readme.md", "package.json", "Gemfile", "pyproject.toml", "requirements.txt",
    "go.mod", "Cargo.toml", "composer.json", "pom.xml", "build.gradle", "mix.exs", "pubspec.yaml",
    "Dockerfile", "docker-compose.yml", "compose.yaml", "Makefile", "Procfile", ".tool-versions",
    "config/routes.rb", "config/database.yml", "app.json", "vercel.json",
];
const MANIFEST_MAX_CHARS: usize = 8_000;
const MANIFESTS_TOTAL_MAX: usize = 40_000;
const TREE_MAX_LINES: usize = 1_500;
/// Used only for estimates.
const SUMMARY_TOKENS_PER_MODULE: u64 = 700;
const SUMMARY_KEYS: &[&str] = &["purpose", "key_files", "public_interface", "depends_on", "used_by", "data", "risks", "notes"];
const DOCS: [(&str, &str); 3] = [
    ("ARCHITECTURE.md", prompts::ARCHITECTURE_TASK),
    ("FLOWS.md", prompts::FLOWS_TASK),
    ("ONBOARDING.md", prompts::ONBOARDING_TASK),
];

#[derive(Debug)]
pub struct Plan {
    pub files: Vec<SourceFile>,
    pub skipped: BTreeMap<String, usize>,
    pub modules: Vec<Module>,
    /// Input tokens for a run with no cache.
    pub estimated_input_tokens: u64,
}

/// What a run will actually send, given the cache.
#[derive(Debug, PartialEq)]
pub struct Pending {
    pub cached_modules: usize,
    pub estimated_input_tokens: u64,
}

pub struct RunResult {
    pub plan: Plan,
    /// One JSON object per module, in module order.
    pub summaries: Vec<Map<String, Value>>,
    /// File name → Markdown.
    pub documents: BTreeMap<String, String>,
    pub failures: Vec<String>,
    /// Entries to persist for the next run (only those used by this one).
    pub cache_entries: BTreeMap<String, Value>,
    pub reused_modules: usize,
    pub reused_documents: usize,
}

/// One module's prompt, built once and reused for the estimate and the call.
struct Prepared {
    user: String,
    key: String,
}

pub struct Analyzer<'a> {
    root: PathBuf,
    repo_name: String,
    config: &'a Config,
    quiet: bool,
    prev: BTreeMap<String, Value>,
}

impl<'a> Analyzer<'a> {
    pub fn new(root: impl Into<PathBuf>, repo_name: &str, config: &'a Config) -> Self {
        Self { root: root.into(), repo_name: repo_name.into(), config, quiet: false, prev: BTreeMap::new() }
    }

    pub fn quiet(mut self) -> Self {
        self.quiet = true;
        self
    }

    /// Entries from a previous run's cache.
    pub fn with_cache(mut self, entries: BTreeMap<String, Value>) -> Self {
        self.prev = entries;
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

    /// How much this run would send, after the cache. When every module is
    /// unchanged the overview inputs are known exactly and checked too;
    /// otherwise the overviews are counted, since new summaries change them.
    pub fn pending(&self, plan: &Plan) -> Pending {
        let prepared = self.prepare(&plan.modules);
        self.pending_for(plan, &prepared)
    }

    fn pending_for(&self, plan: &Plan, prepared: &[Prepared]) -> Pending {
        let overhead = estimate_tokens(prompts::MODULE_SYSTEM.len() as u64);
        let cached = |p: &Prepared| self.prev.get(&p.key).is_some_and(Value::is_object);
        let modules: u64 = prepared
            .iter()
            .filter(|p| !cached(p))
            .map(|p| estimate_tokens(p.user.len() as u64) + overhead)
            .sum();
        let cached_modules = prepared.iter().filter(|p| cached(p)).count();

        let docs = if cached_modules == prepared.len() {
            let summaries: Vec<_> = plan
                .modules
                .iter()
                .zip(prepared)
                .map(|(m, p)| with_identity(self.prev[&p.key].as_object().cloned().unwrap_or_default(), m))
                .collect();
            self.doc_inputs(plan, &summaries)
                .iter()
                .filter(|(_, _, key)| !self.prev.contains_key(key))
                .map(|(_, user, _)| estimate_tokens((user.len() + prompts::SYNTHESIS_SYSTEM.len()) as u64))
                .sum()
        } else {
            synthesis_once(&plan.files, plan.modules.len()) * DOCS.len() as u64
        };
        Pending { cached_modules, estimated_input_tokens: modules + docs }
    }

    pub fn run(&self, client: &dyn Llm) -> Result<RunResult> {
        let plan = self.plan()?;
        let prepared = self.prepare(&plan.modules);
        let pending = self.pending_for(&plan, &prepared);
        if pending.estimated_input_tokens > self.config.max_input_tokens {
            bail!(
                "Estimated {} input tokens exceeds the limit of {}. Raise --max-input-tokens to proceed.",
                pending.estimated_input_tokens,
                self.config.max_input_tokens
            );
        }

        let fresh = plan.modules.len() - pending.cached_modules;
        if pending.cached_modules > 0 {
            self.say(&format!(
                "Summarising {fresh} changed modules with {} ({} unchanged, from cache)…",
                self.config.fast_model, pending.cached_modules
            ));
        } else {
            self.say(&format!("Summarising {} modules with {}…", plan.modules.len(), self.config.fast_model));
        }
        let mut cache_entries = BTreeMap::new();
        let (summaries, failures, reused_modules) = self.summarise_modules(&plan.modules, &prepared, client, &mut cache_entries);

        self.say(&format!("Writing architecture, flows and onboarding with {}…", self.config.smart_model));
        let (documents, reused_documents) = self.synthesise(&plan, &summaries, client, &mut cache_entries);

        Ok(RunResult { plan, summaries, documents, failures, cache_entries, reused_modules, reused_documents })
    }

    fn prepare(&self, modules: &[Module]) -> Vec<Prepared> {
        modules
            .iter()
            .map(|m| {
                let text = m
                    .files
                    .iter()
                    .map(|f| format!("=== {} ({}) ===\n{}", f.path, f.language, f.read(&self.root)))
                    .collect::<Vec<_>>()
                    .join("\n\n");
                let user = prompts::module_user(&m.name, &text, self.config.lang);
                let key = cache::key(&["module", &self.config.fast_model, &prompts::MODULE_SYSTEM, &user]);
                Prepared { user, key }
            })
            .collect()
    }

    fn summarise_modules(
        &self,
        modules: &[Module],
        prepared: &[Prepared],
        client: &dyn Llm,
        cache_out: &mut BTreeMap<String, Value>,
    ) -> (Vec<Map<String, Value>>, Vec<String>, usize) {
        let next = AtomicUsize::new(0);
        let done = AtomicUsize::new(0);
        let reused = AtomicUsize::new(0);
        let results: Mutex<Vec<Option<Map<String, Value>>>> = Mutex::new(vec![None; modules.len()]);
        let workers = self.config.concurrency.max(1).min(modules.len().max(1));
        let total = modules.len() - pending_cached(prepared, &self.prev);

        std::thread::scope(|s| {
            for _ in 0..workers {
                s.spawn(|| loop {
                    let i = next.fetch_add(1, Ordering::SeqCst);
                    let Some(m) = modules.get(i) else { break };
                    let p = &prepared[i];
                    let (summary, from_cache) = match self.prev.get(&p.key).and_then(Value::as_object) {
                        Some(hit) => (with_identity(hit.clone(), m), true),
                        None => (self.summarise(m, &p.user, client), false),
                    };
                    let failed = summary.contains_key("error");
                    results.lock().unwrap()[i] = Some(summary);
                    if from_cache {
                        reused.fetch_add(1, Ordering::SeqCst);
                    } else {
                        let n = done.fetch_add(1, Ordering::SeqCst) + 1;
                        self.say(&format!("  [{n}/{total}] {}{}", m.name, if failed { " (failed)" } else { "" }));
                    }
                });
            }
        });

        let summaries: Vec<_> = results.into_inner().unwrap().into_iter().map(Option::unwrap).collect();
        for (s, p) in summaries.iter().zip(prepared) {
            if !s.contains_key("error") {
                cache_out.insert(p.key.clone(), Value::Object(s.clone()));
            }
        }
        let failures = summaries
            .iter()
            .filter(|s| s.contains_key("error"))
            .map(|s| s["module"].as_str().unwrap_or_default().to_string())
            .collect();
        (summaries, failures, reused.into_inner())
    }

    fn summarise(&self, m: &Module, user: &str, client: &dyn Llm) -> Map<String, Value> {
        let mut out = with_identity(Map::new(), m);
        let reply = match client.complete(&self.config.fast_model, &prompts::MODULE_SYSTEM, user, 4_000) {
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

    /// (file name, user prompt, cache key) for each overview document.
    fn doc_inputs(&self, plan: &Plan, summaries: &[Map<String, Value>]) -> Vec<(&'static str, String, String)> {
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
        DOCS.iter()
            .map(|(file, task)| {
                let user = prompts::synthesis_user(&self.repo_name, &tree, &manifests, &modules_json, task, self.config.lang);
                let key = cache::key(&["doc", &self.config.smart_model, &prompts::SYNTHESIS_SYSTEM, &user]);
                (*file, user, key)
            })
            .collect()
    }

    fn synthesise(
        &self,
        plan: &Plan,
        summaries: &[Map<String, Value>],
        client: &dyn Llm,
        cache_out: &mut BTreeMap<String, Value>,
    ) -> (BTreeMap<String, String>, usize) {
        let labels = self.config.lang.labels();
        let inputs = self.doc_inputs(plan, summaries);

        let results: Vec<(String, String, String, bool, bool)> = std::thread::scope(|s| {
            let handles: Vec<_> = inputs
                .into_iter()
                .map(|(file, user, key)| {
                    let hit = self.prev.get(&key).and_then(Value::as_str).map(String::from);
                    s.spawn(move || {
                        if let Some(text) = hit {
                            return (file.to_string(), text, key, true, true);
                        }
                        match client.complete(&self.config.smart_model, &prompts::SYNTHESIS_SYSTEM, &user, 8_000) {
                            Ok(t) => (file.to_string(), strip_outer_fence(&t), key, true, false),
                            Err(e) => (
                                file.to_string(),
                                format!("# {}\n\n{}: {e}\n", file.trim_end_matches(".md"), labels.doc_failed),
                                key,
                                false,
                                false,
                            ),
                        }
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });

        let mut docs = BTreeMap::new();
        let mut reused = 0;
        for (file, text, key, ok, from_cache) in results {
            if ok {
                cache_out.insert(key, Value::String(text.clone()));
            }
            reused += usize::from(from_cache);
            docs.insert(file, text);
        }
        (docs, reused)
    }

    fn say(&self, msg: &str) {
        if !self.quiet {
            eprintln!("{msg}");
        }
    }
}

/// The derived fields always reflect the current module, cached or not.
fn with_identity(mut s: Map<String, Value>, m: &Module) -> Map<String, Value> {
    s.insert("module".into(), json!(m.name));
    s.insert("slug".into(), json!(m.slug()));
    s.insert("files".into(), json!(m.files.iter().map(|f| f.path.clone()).collect::<Vec<_>>()));
    s
}

fn pending_cached(prepared: &[Prepared], prev: &BTreeMap<String, Value>) -> usize {
    prepared.iter().filter(|p| prev.get(&p.key).is_some_and(Value::is_object)).count()
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
    let mut lines: Vec<String> = files.iter().map(|f| f.path.clone()).take(TREE_MAX_LINES).collect();
    let extra = files.len().saturating_sub(TREE_MAX_LINES);
    if extra > 0 {
        lines.push(format!("… {extra} more files"));
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

fn synthesis_once(files: &[SourceFile], modules: usize) -> u64 {
    estimate_tokens(files.iter().map(|f| f.path.len() as u64 + 1).sum())
        + estimate_tokens(MANIFESTS_TOTAL_MAX as u64) / 2
        + modules as u64 * SUMMARY_TOKENS_PER_MODULE
        + estimate_tokens(prompts::SYNTHESIS_SYSTEM.len() as u64)
}

fn estimate(files: &[SourceFile], modules: &[Module]) -> u64 {
    let prompt_overhead = estimate_tokens(prompts::MODULE_SYSTEM.len() as u64) + 50;
    let module_tokens = estimate_tokens(files.iter().map(SourceFile::prompt_size).sum()) + modules.len() as u64 * prompt_overhead;
    module_tokens + synthesis_once(files, modules.len()) * DOCS.len() as u64
}
