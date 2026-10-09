//! Runs the pipeline for one repository:
//! 1. Summarise each module in parallel with the fast model (structured JSON).
//! 2. For large repositories, roll module summaries up into areas (folders),
//!    again with the fast model, so no later call has to read hundreds of them.
//! 3. Write ARCHITECTURE, FLOWS and ONBOARDING with the smart model, from the
//!    file tree, manifests and compact summaries (never from raw code).
//!
//! Every call consults the [`CacheStore`]: a call whose exact input was seen
//! before is answered from the cache. Progress is saved as the run goes, and a
//! fatal error (usage limit, not logged in, bad key) stops further calls at
//! once instead of failing every remaining module.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Map, Value};

use crate::cache::{self, CacheStore};
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
/// Above this many files the overview gets folder counts instead of every path.
const TREE_FULL_MAX_FILES: usize = 800;
const TREE_MAX_LINES: usize = 1_500;
const INDEX_MAX_LINES: usize = 1_500;
/// Used only for estimates.
const COMPACT_TOKENS_PER_MODULE: u64 = 250;
const AREA_TOKENS: u64 = 450;
/// Sub-areas smaller than this are merged with their siblings.
const MIN_AREA_MODULES: usize = 3;
const SUMMARY_KEYS: &[&str] = &["purpose", "key_files", "public_interface", "depends_on", "used_by", "data", "risks", "notes"];
const AREA_KEYS: &[&str] = &["purpose", "components", "interfaces", "depends_on", "data", "risks"];
pub const DOCS: [(&str, &str); 3] = [
    ("ARCHITECTURE.md", prompts::ARCHITECTURE_TASK),
    ("FLOWS.md", prompts::FLOWS_TASK),
    ("ONBOARDING.md", prompts::ONBOARDING_TASK),
];

pub use crate::client::is_fatal;

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
    /// Area roll-ups (empty for small repositories).
    pub areas: Vec<Map<String, Value>>,
    /// File name → Markdown. Empty when the run stopped early.
    pub documents: BTreeMap<String, String>,
    pub failures: Vec<String>,
    pub reused_modules: usize,
    pub reused_documents: usize,
    /// Set when a fatal error stopped the run; finished calls are cached.
    pub stopped: Option<String>,
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
    store: Option<&'a CacheStore>,
    stop: Mutex<Option<String>>,
}

impl<'a> Analyzer<'a> {
    pub fn new(root: impl Into<PathBuf>, repo_name: &str, config: &'a Config) -> Self {
        Self { root: root.into(), repo_name: repo_name.into(), config, quiet: false, store: None, stop: Mutex::new(None) }
    }

    pub fn quiet(mut self) -> Self {
        self.quiet = true;
        self
    }

    pub fn with_store(mut self, store: &'a CacheStore) -> Self {
        self.store = Some(store);
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Everything that can be known without calling Claude.
    pub fn plan(&self) -> Result<Plan> {
        let ScanResult { files, skipped } = Scanner::with_options(&self.root, self.config.scan.clone())?.scan();
        if files.is_empty() {
            bail!("No source files found to document in {}.", self.repo_name);
        }
        if files.len() > self.config.max_files {
            bail!(
                "{}: {} source files after filtering; the limit is {}. Narrow it with --include/--exclude/--skip-tests, \
                 or raise --max-files.",
                self.repo_name,
                files.len(),
                self.config.max_files
            );
        }
        let modules = Grouper::new(self.config.module_char_budget, self.config.min_module_chars).group(&files);
        let estimated_input_tokens = self.estimate(&files, &modules);
        Ok(Plan { files, skipped, modules, estimated_input_tokens })
    }

    /// How much this run would send after the cache. When every module is
    /// unchanged the later stages are checked exactly too.
    pub fn pending(&self, plan: &Plan) -> Pending {
        let prepared = self.prepare(&plan.modules);
        self.pending_for(plan, &prepared)
    }

    /// Runs with a fresh plan, checking it against the budget first.
    pub fn run(&self, client: &dyn Llm) -> Result<RunResult> {
        let plan = self.plan()?;
        let prepared = self.prepare(&plan.modules);
        let pending = self.pending_for(&plan, &prepared);
        if pending.estimated_input_tokens > self.config.max_input_tokens {
            bail!(
                "{}: estimated {} input tokens exceeds the limit of {}. Narrow it with --include/--exclude/--skip-tests, \
                 or raise --max-input-tokens.",
                self.repo_name,
                pending.estimated_input_tokens,
                self.config.max_input_tokens
            );
        }
        Ok(self.run_plan(plan, &prepared, pending.cached_modules, client))
    }

    /// Runs an already-checked plan (used when several repositories share one budget).
    pub fn run_checked(&self, plan: Plan, client: &dyn Llm) -> RunResult {
        let prepared = self.prepare(&plan.modules);
        let cached = prepared.iter().filter(|p| self.cached(&p.key)).count();
        self.run_plan(plan, &prepared, cached, client)
    }

    fn run_plan(&self, plan: Plan, prepared: &[Prepared], cached_modules: usize, client: &dyn Llm) -> RunResult {
        let fresh = plan.modules.len() - cached_modules;
        self.say(&format!(
            "[{}] Summarising {fresh} of {} modules with {}{}…",
            self.repo_name,
            plan.modules.len(),
            self.config.fast_model,
            if cached_modules > 0 { format!(" ({cached_modules} unchanged, from cache)") } else { String::new() }
        ));
        let (summaries, failures, reused_modules) = self.summarise_modules(&plan.modules, prepared, client);
        let mut result = RunResult {
            plan,
            summaries,
            areas: vec![],
            documents: BTreeMap::new(),
            failures,
            reused_modules,
            reused_documents: 0,
            stopped: None,
        };
        if self.stop_reason(&mut result) {
            return result;
        }

        if self.rollup(&result.plan) {
            let groups = area_groups(&result.plan.modules, self.config.area_max_modules);
            self.say(&format!("[{}] Rolling {} modules up into {} areas…", self.repo_name, result.plan.modules.len(), groups.len()));
            result.areas = self.summarise_areas(&groups, &result.summaries, client);
            if self.stop_reason(&mut result) {
                return result;
            }
        }

        self.say(&format!("[{}] Writing architecture, flows and onboarding with {}…", self.repo_name, self.config.smart_model));
        let (documents, reused) = self.synthesise(&result.plan, &result.summaries, &result.areas, client);
        result.reused_documents = reused;
        if !self.stop_reason(&mut result) {
            result.documents = documents;
        }
        result
    }

    fn stop_reason(&self, result: &mut RunResult) -> bool {
        result.stopped = self.stop.lock().unwrap().clone();
        result.stopped.is_some()
    }

    fn rollup(&self, plan: &Plan) -> bool {
        plan.modules.len() > self.config.rollup_threshold
    }

    // ---- cache helpers -----------------------------------------------------

    fn cached(&self, key: &str) -> bool {
        self.store.is_some_and(|s| s.contains(key))
    }

    fn peek(&self, key: &str) -> Option<Value> {
        self.store.and_then(|s| s.peek(key))
    }

    fn get(&self, key: &str) -> Option<Value> {
        self.store.and_then(|s| s.get(key))
    }

    fn put(&self, key: &str, v: Value) {
        if let Some(s) = self.store {
            s.put(key.to_string(), v);
        }
    }

    /// Calls Claude; a fatal error is recorded so no further calls start.
    fn call(&self, model: &str, system: &str, user: &str, max_tokens: u32, client: &dyn Llm) -> Result<String> {
        if let Some(r) = self.stop.lock().unwrap().clone() {
            bail!("skipped: run stopped ({r})");
        }
        client.complete(model, system, user, max_tokens).inspect_err(|e| {
            let msg = e.to_string();
            if is_fatal(&msg) {
                self.stop.lock().unwrap().get_or_insert(msg);
            }
        })
    }

    // ---- stage 1: modules --------------------------------------------------

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
    ) -> (Vec<Map<String, Value>>, Vec<String>, usize) {
        let next = AtomicUsize::new(0);
        let done = AtomicUsize::new(0);
        let reused = AtomicUsize::new(0);
        let results: Mutex<Vec<Option<Map<String, Value>>>> = Mutex::new(vec![None; modules.len()]);
        let workers = self.config.concurrency.max(1).min(modules.len().max(1));
        let total = prepared.iter().filter(|p| !self.cached(&p.key)).count();

        std::thread::scope(|s| {
            for _ in 0..workers {
                s.spawn(|| loop {
                    let i = next.fetch_add(1, Ordering::SeqCst);
                    let Some(m) = modules.get(i) else { break };
                    let p = &prepared[i];
                    let summary = match self.get(&p.key).and_then(|v| v.as_object().cloned()) {
                        Some(hit) => {
                            reused.fetch_add(1, Ordering::SeqCst);
                            with_identity(hit, m)
                        }
                        None => {
                            let skipped = self.stop.lock().unwrap().is_some();
                            let s = self.summarise(m, &p.user, client);
                            if !s.contains_key("error") {
                                self.put(&p.key, Value::Object(s.clone()));
                            }
                            if !skipped {
                                let n = done.fetch_add(1, Ordering::SeqCst) + 1;
                                let failed = if s.contains_key("error") { " (failed)" } else { "" };
                                self.say(&format!("  [{}] {n}/{total} {}{failed}", self.repo_name, m.name));
                            }
                            s
                        }
                    };
                    results.lock().unwrap()[i] = Some(summary);
                });
            }
        });

        let summaries: Vec<_> = results.into_inner().unwrap().into_iter().map(Option::unwrap).collect();
        let failures = summaries
            .iter()
            .filter(|s| s.contains_key("error"))
            .map(|s| s["module"].as_str().unwrap_or_default().to_string())
            .collect();
        (summaries, failures, reused.into_inner())
    }

    fn summarise(&self, m: &Module, user: &str, client: &dyn Llm) -> Map<String, Value> {
        let mut out = with_identity(Map::new(), m);
        let reply = match self.call(&self.config.fast_model, &prompts::MODULE_SYSTEM, user, 4_000, client) {
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

    // ---- stage 2: areas (large repositories) -------------------------------

    /// (area name, user prompt, cache key, member indexes). Single-module areas
    /// need no call and get an empty prompt.
    fn area_inputs(&self, groups: &[(String, Vec<usize>)], summaries: &[Map<String, Value>]) -> Vec<(String, String, String, Vec<usize>)> {
        groups
            .iter()
            .map(|(name, idx)| {
                if idx.len() == 1 {
                    return (name.clone(), String::new(), String::new(), idx.clone());
                }
                let members: Vec<Value> =
                    idx.iter().filter(|&&i| !summaries[i].contains_key("error")).map(|&i| compact(&summaries[i])).collect();
                let user = prompts::area_user(name, &serde_json::to_string(&members).unwrap_or_default(), self.config.lang);
                let key = cache::key(&["area", &self.config.fast_model, &prompts::AREA_SYSTEM, &user]);
                (name.clone(), user, key, idx.clone())
            })
            .collect()
    }

    fn summarise_areas(&self, groups: &[(String, Vec<usize>)], summaries: &[Map<String, Value>], client: &dyn Llm) -> Vec<Map<String, Value>> {
        let inputs = self.area_inputs(groups, summaries);
        let next = AtomicUsize::new(0);
        let results: Mutex<Vec<Option<Map<String, Value>>>> = Mutex::new(vec![None; inputs.len()]);
        let workers = self.config.concurrency.max(1).min(inputs.len().max(1));

        std::thread::scope(|s| {
            for _ in 0..workers {
                s.spawn(|| loop {
                    let i = next.fetch_add(1, Ordering::SeqCst);
                    let Some((name, user, key, idx)) = inputs.get(i) else { break };
                    let members: Vec<&Map<String, Value>> = idx.iter().map(|&j| &summaries[j]).collect();
                    let area = if user.is_empty() {
                        area_from_single(members[0])
                    } else if let Some(hit) = self.get(key).and_then(|v| v.as_object().cloned()) {
                        hit
                    } else {
                        match self.call(&self.config.fast_model, &prompts::AREA_SYSTEM, user, 3_000, client).map(|r| parse_json_object(&r)) {
                            Ok(Ok(Value::Object(obj))) => {
                                let a: Map<String, Value> =
                                    AREA_KEYS.iter().filter_map(|k| obj.get(*k).map(|v| ((*k).to_string(), v.clone()))).collect();
                                self.put(key, Value::Object(a.clone()));
                                a
                            }
                            // A failed roll-up degrades to a plain list; the run goes on.
                            _ => area_fallback(&members),
                        }
                    };
                    results.lock().unwrap()[i] = Some(with_area_identity(area, name, &members));
                });
            }
        });
        results.into_inner().unwrap().into_iter().map(Option::unwrap).collect()
    }

    // ---- stage 3: overview documents ---------------------------------------

    fn code_block(summaries: &[Map<String, Value>], areas: &[Map<String, Value>]) -> String {
        let ok: Vec<&Map<String, Value>> = summaries.iter().filter(|s| !s.contains_key("error")).collect();
        if areas.is_empty() {
            let compacts: Vec<Value> = ok.iter().map(|s| compact(s)).collect();
            return format!("<module_summaries>\n{}\n</module_summaries>", serde_json::to_string_pretty(&compacts).unwrap_or_default());
        }
        let mut index: Vec<String> = ok
            .iter()
            .map(|s| format!("{} | {} | {}", str_of(s, "slug"), str_of(s, "module"), first_sentence(&str_of(s, "purpose"))))
            .collect();
        if index.len() > INDEX_MAX_LINES {
            let extra = index.len() - INDEX_MAX_LINES;
            index.truncate(INDEX_MAX_LINES);
            index.push(format!("… {extra} more modules"));
        }
        format!(
            "<area_summaries>\n{}\n</area_summaries>\n\n<module_index>\nslug | module | purpose\n{}\n</module_index>",
            serde_json::to_string_pretty(areas).unwrap_or_default(),
            index.join("\n")
        )
    }

    /// (file name, user prompt, cache key) for each overview document.
    fn doc_inputs(&self, plan: &Plan, summaries: &[Map<String, Value>], areas: &[Map<String, Value>]) -> Vec<(&'static str, String, String)> {
        let code = Self::code_block(summaries, areas);
        let tree = tree_text(&plan.files);
        let manifests = manifests_text(&self.root, &plan.files);
        DOCS.iter()
            .map(|(file, task)| {
                let user = prompts::synthesis_user(&self.repo_name, &tree, &manifests, &code, task, self.config.lang);
                let key = cache::key(&["doc", &self.config.smart_model, &prompts::SYNTHESIS_SYSTEM, &user]);
                (*file, user, key)
            })
            .collect()
    }

    fn synthesise(
        &self,
        plan: &Plan,
        summaries: &[Map<String, Value>],
        areas: &[Map<String, Value>],
        client: &dyn Llm,
    ) -> (BTreeMap<String, String>, usize) {
        let labels = self.config.lang.labels();
        let inputs = self.doc_inputs(plan, summaries, areas);
        let results: Vec<(String, String, bool)> = std::thread::scope(|s| {
            let handles: Vec<_> = inputs
                .into_iter()
                .map(|(file, user, key)| {
                    s.spawn(move || {
                        if let Some(text) = self.get(&key).and_then(|v| v.as_str().map(String::from)) {
                            return (file.to_string(), text, true);
                        }
                        let text = match self.call(&self.config.smart_model, &prompts::SYNTHESIS_SYSTEM, &user, 8_000, client) {
                            Ok(t) => {
                                let t = strip_outer_fence(&t);
                                self.put(&key, Value::String(t.clone()));
                                t
                            }
                            Err(e) => format!("# {}\n\n{}: {e}\n", file.trim_end_matches(".md"), labels.doc_failed),
                        };
                        (file.to_string(), text, false)
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let reused = results.iter().filter(|r| r.2).count();
        (results.into_iter().map(|(f, t, _)| (f, t)).collect(), reused)
    }

    // ---- estimates ---------------------------------------------------------

    fn pending_for(&self, plan: &Plan, prepared: &[Prepared]) -> Pending {
        let overhead = estimate_tokens(prompts::MODULE_SYSTEM.len() as u64);
        let modules: u64 =
            prepared.iter().filter(|p| !self.cached(&p.key)).map(|p| estimate_tokens(p.user.len() as u64) + overhead).sum();
        let cached_modules = prepared.iter().filter(|p| self.cached(&p.key)).count();

        let later = if cached_modules == prepared.len() && !prepared.is_empty() {
            self.exact_later_stages(plan, prepared)
        } else {
            self.areas_estimate(plan) + self.docs_estimate(plan)
        };
        Pending { cached_modules, estimated_input_tokens: modules + later }
    }

    /// With every module cached, rebuild the area and document inputs and
    /// count only the calls that would miss the cache.
    fn exact_later_stages(&self, plan: &Plan, prepared: &[Prepared]) -> u64 {
        let summaries: Vec<_> = plan
            .modules
            .iter()
            .zip(prepared)
            .map(|(m, p)| with_identity(self.peek(&p.key).and_then(|v| v.as_object().cloned()).unwrap_or_default(), m))
            .collect();
        let mut areas = vec![];
        let mut area_tokens = 0;
        if self.rollup(plan) {
            let groups = area_groups(&plan.modules, self.config.area_max_modules);
            for (name, user, key, idx) in self.area_inputs(&groups, &summaries) {
                let members: Vec<&Map<String, Value>> = idx.iter().map(|&j| &summaries[j]).collect();
                let area = if user.is_empty() {
                    area_from_single(members[0])
                } else if let Some(hit) = self.peek(&key).and_then(|v| v.as_object().cloned()) {
                    hit
                } else {
                    area_tokens += estimate_tokens((user.len() + prompts::AREA_SYSTEM.len()) as u64);
                    Map::new()
                };
                areas.push(with_area_identity(area, &name, &members));
            }
        }
        if area_tokens > 0 {
            // New roll-ups change the document inputs, so count the documents too.
            return area_tokens + self.docs_estimate(plan);
        }
        self.doc_inputs(plan, &summaries, &areas)
            .iter()
            .filter(|(_, _, key)| !self.cached(key))
            .map(|(_, user, _)| estimate_tokens((user.len() + prompts::SYNTHESIS_SYSTEM.len()) as u64))
            .sum()
    }

    fn areas_estimate(&self, plan: &Plan) -> u64 {
        if !self.rollup(plan) {
            return 0;
        }
        area_groups(&plan.modules, self.config.area_max_modules)
            .iter()
            .filter(|(_, idx)| idx.len() > 1)
            .map(|(_, idx)| idx.len() as u64 * COMPACT_TOKENS_PER_MODULE + estimate_tokens(prompts::AREA_SYSTEM.len() as u64))
            .sum()
    }

    fn docs_estimate(&self, plan: &Plan) -> u64 {
        let tree = estimate_tokens(tree_text(&plan.files).len() as u64);
        let code = if self.rollup(plan) {
            area_groups(&plan.modules, self.config.area_max_modules).len() as u64 * AREA_TOKENS
                + plan.modules.len().min(INDEX_MAX_LINES) as u64 * 25
        } else {
            plan.modules.len() as u64 * COMPACT_TOKENS_PER_MODULE
        };
        let once = tree + estimate_tokens(MANIFESTS_TOTAL_MAX as u64) / 2 + code + estimate_tokens(prompts::SYNTHESIS_SYSTEM.len() as u64);
        once * DOCS.len() as u64
    }

    fn estimate(&self, files: &[SourceFile], modules: &[Module]) -> u64 {
        let overhead = estimate_tokens(prompts::MODULE_SYSTEM.len() as u64) + 50;
        let module_tokens = estimate_tokens(files.iter().map(SourceFile::prompt_size).sum()) + modules.len() as u64 * overhead;
        let plan = Plan { files: files.to_vec(), skipped: BTreeMap::new(), modules: modules.to_vec(), estimated_input_tokens: 0 };
        module_tokens + self.areas_estimate(&plan) + self.docs_estimate(&plan)
    }

    fn say(&self, msg: &str) {
        if !self.quiet {
            eprintln!("{msg}");
        }
    }
}

// ---- helpers ---------------------------------------------------------------

/// The derived fields always reflect the current module, cached or not.
fn with_identity(mut s: Map<String, Value>, m: &Module) -> Map<String, Value> {
    s.insert("module".into(), json!(m.name));
    s.insert("slug".into(), json!(m.slug()));
    s.insert("files".into(), json!(m.files.iter().map(|f| f.path.clone()).collect::<Vec<_>>()));
    s
}

fn with_area_identity(mut a: Map<String, Value>, name: &str, members: &[&Map<String, Value>]) -> Map<String, Value> {
    a.insert("area".into(), json!(name));
    a.insert("modules".into(), json!(members.iter().map(|m| str_of(m, "slug")).collect::<Vec<_>>()));
    a
}

fn take(s: &Map<String, Value>, key: &str, n: usize) -> Value {
    Value::Array(s.get(key).and_then(Value::as_array).map(|a| a.iter().take(n).cloned().collect()).unwrap_or_default())
}

/// The fields later stages need, trimmed.
pub fn compact(s: &Map<String, Value>) -> Value {
    let key_files: Vec<Value> = s
        .get("key_files")
        .and_then(Value::as_array)
        .map(|a| a.iter().take(5).filter_map(|k| k.get("path").cloned()).collect())
        .unwrap_or_default();
    json!({
        "module": s.get("module"),
        "slug": s.get("slug"),
        "purpose": s.get("purpose"),
        "key_files": key_files,
        "public_interface": take(s, "public_interface", 8),
        "depends_on": take(s, "depends_on", 8),
        "data": take(s, "data", 5),
        "risks": take(s, "risks", 3),
    })
}

fn area_from_single(m: &Map<String, Value>) -> Map<String, Value> {
    let mut a = Map::new();
    a.insert("purpose".into(), m.get("purpose").cloned().unwrap_or(json!("")));
    a.insert("components".into(), json!([{ "module": m.get("module"), "role": first_sentence(&str_of(m, "purpose")) }]));
    a.insert("interfaces".into(), take(m, "public_interface", 10));
    a.insert("depends_on".into(), take(m, "depends_on", 10));
    a.insert("data".into(), take(m, "data", 5));
    a.insert("risks".into(), take(m, "risks", 5));
    a
}

fn area_fallback(members: &[&Map<String, Value>]) -> Map<String, Value> {
    let components: Vec<Value> = members
        .iter()
        .take(15)
        .map(|m| json!({ "module": m.get("module"), "role": first_sentence(&str_of(m, "purpose")) }))
        .collect();
    let mut a = Map::new();
    a.insert("purpose".into(), json!(""));
    a.insert("components".into(), Value::Array(components));
    a
}

/// Groups modules into areas of at most `max` modules: by top-level folder,
/// splitting any area that is too big one folder level deeper. Tests form one
/// area; root files and small folders form another.
pub fn area_groups(modules: &[Module], max: usize) -> Vec<(String, Vec<usize>)> {
    fn key(name: &str, depth: usize) -> String {
        let base = name.find(" (part ").map_or(name, |i| &name[..i]);
        if base.ends_with(" (tests)") {
            return if depth <= 1 { "(tests)".into() } else { base.to_string() };
        }
        if base.starts_with('(') {
            return "(root)".into();
        }
        let segs: Vec<&str> = base.split('/').collect();
        segs[..depth.min(segs.len())].join("/")
    }
    fn split(modules: &[Module], idx: Vec<usize>, depth: usize, max: usize, out: &mut Vec<(String, Vec<usize>)>) {
        let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
        for i in idx {
            let k = key(&modules[i].name, depth);
            match groups.iter_mut().find(|(g, _)| *g == k) {
                Some((_, v)) => v.push(i),
                None => groups.push((k, vec![i])),
            }
        }
        for (name, members) in groups {
            let deeper: BTreeSet<String> = members.iter().map(|&i| key(&modules[i].name, depth + 1)).collect();
            if members.len() > max && deeper.len() > 1 {
                // Split one level deeper, then gather the tiny sub-areas back
                // together so the roll-up does not degrade to one area per module.
                let mut children = Vec::new();
                split(modules, members, depth + 1, max, &mut children);
                let (tiny, big): (Vec<_>, Vec<_>) = children.into_iter().partition(|(_, m)| m.len() < MIN_AREA_MODULES);
                out.extend(big);
                let rest: Vec<usize> = tiny.into_iter().flat_map(|(_, m)| m).collect();
                for (n, chunk) in rest.chunks(max).enumerate() {
                    let label = if rest.len() > max { format!("{name} (other {})", n + 1) } else { format!("{name} (other)") };
                    out.push((label, chunk.to_vec()));
                }
            } else if members.len() > max {
                for (n, chunk) in members.chunks(max).enumerate() {
                    out.push((format!("{name} (group {})", n + 1), chunk.to_vec()));
                }
            } else {
                out.push((name, members));
            }
        }
    }
    let max = max.max(2);
    let mut top = Vec::new();
    split(modules, (0..modules.len()).collect(), 1, max, &mut top);
    // Tiny top-level folders (.github, docs, script…) are gathered into one area.
    let (tiny, mut out): (Vec<_>, Vec<_>) =
        top.into_iter().partition(|(name, m)| m.len() < MIN_AREA_MODULES && !name.starts_with('('));
    let rest: Vec<usize> = tiny.into_iter().flat_map(|(_, m)| m).collect();
    if rest.len() == 1 {
        out.push((modules[rest[0]].name.clone(), rest));
    } else {
        for (n, chunk) in rest.chunks(max).enumerate() {
            out.push((if rest.len() > max { format!("(other {})", n + 1) } else { "(other)".into() }, chunk.to_vec()));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
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

pub fn strip_outer_fence(text: &str) -> String {
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

/// Every path for small repositories; folders with file counts for large ones.
pub fn tree_text(files: &[SourceFile]) -> String {
    if files.len() <= TREE_FULL_MAX_FILES {
        return files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>().join("\n");
    }
    let mut dirs: BTreeMap<String, usize> = BTreeMap::new();
    let mut root_files = Vec::new();
    for f in files {
        let parts: Vec<&str> = f.path.split('/').collect();
        if parts.len() == 1 {
            root_files.push(f.path.as_str());
            continue;
        }
        for d in 1..=(parts.len() - 1).min(3) {
            *dirs.entry(format!("{}/", parts[..d].join("/"))).or_default() += 1;
        }
    }
    let mut lines = vec![format!("({} files; folders shown to depth 3 with file counts)", files.len())];
    lines.extend(root_files.iter().take(50).map(|s| s.to_string()));
    lines.extend(dirs.iter().map(|(d, n)| format!("{d} ({n})")));
    if lines.len() > TREE_MAX_LINES {
        let extra = lines.len() - TREE_MAX_LINES;
        lines.truncate(TREE_MAX_LINES);
        lines.push(format!("… {extra} more folders"));
    }
    lines.join("\n")
}

pub fn manifests_text(root: &Path, files: &[SourceFile]) -> String {
    let mut total = 0;
    let mut parts = Vec::new();
    for f in files.iter().filter(|f| MANIFEST_NAMES.contains(&f.path.as_str())) {
        if total >= MANIFESTS_TOTAL_MAX {
            break;
        }
        let whole = SourceFile { mode: Default::default(), ..f.clone() };
        let body: String = whole.read(root).chars().take(MANIFEST_MAX_CHARS).collect();
        total += body.len();
        parts.push(format!("=== {} ===\n{body}", f.path));
    }
    if parts.is_empty() { "(none found)".into() } else { parts.join("\n\n") }
}

pub fn str_of(s: &Map<String, Value>, key: &str) -> String {
    match s.get(key) {
        Some(Value::String(v)) => v.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

pub fn first_sentence(text: &str) -> String {
    let t = text.split_whitespace().collect::<Vec<_>>().join(" ").replace('|', "\\|");
    let bytes = t.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if matches!(b, b'.' | b'!' | b'?') && (i + 1 == bytes.len() || bytes[i + 1] == b' ') {
            return t[..=i].to_string();
        }
    }
    // Japanese text has no spaced full stops.
    if let Some(i) = t.find('。') {
        return t[..i + '。'.len_utf8()].to_string();
    }
    t
}
