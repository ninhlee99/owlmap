//! Several repositories that form one system (for example a candidate site,
//! a company admin and a shared API). Each repository gets its own docs; this
//! module writes the parts that span them: SYSTEM.md, describing how the
//! repositories connect, and a workspace README.
//!
//! Connections are grounded in evidence pulled from the code without Claude:
//! the environment variables each repository reads, the hosts it calls, and
//! where it mentions the other repositories by name.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use anyhow::Result;
use regex::Regex;
use serde_json::Value;

use crate::analyzer::{first_sentence, str_of, strip_outer_fence, RunResult};
use crate::cache::{self, CacheStore};
use crate::client::Llm;
use crate::config::Config;
use crate::estimate_tokens;
use crate::prompts;
use crate::scanner::SourceFile;

const ARCHITECTURE_MAX_CHARS: usize = 10_000;
const INTERFACE_MAX_LINES: usize = 150;
const SIGNALS_PER_KIND: usize = 40;
const EXAMPLES_PER_SIGNAL: usize = 3;

pub struct Repo<'a> {
    pub name: String,
    pub root: PathBuf,
    pub url: Option<String>,
    pub commit: Option<String>,
    pub result: &'a RunResult,
}

/// Evidence of how a repository talks to the outside world.
#[derive(Debug, Default, PartialEq)]
pub struct Signals {
    /// Name → files that read it.
    pub env: BTreeMap<String, BTreeSet<String>>,
    /// Host → files that reference it.
    pub hosts: BTreeMap<String, BTreeSet<String>>,
    /// Other repository name → files that mention it.
    pub mentions: BTreeMap<String, BTreeSet<String>>,
}

static ENV_READ: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r#"ENV(?:\.fetch\(|\[)\s*["']([A-Z][A-Z0-9_]{2,})["']"#,
        r#"|process\.env\.([A-Z][A-Z0-9_]{2,})|process\.env\[\s*["']([A-Z][A-Z0-9_]{2,})["']"#,
        r#"|import\.meta\.env\.([A-Z][A-Z0-9_]{2,})"#,
        r#"|(?:getenv|environ\.get|env)\(\s*["']([A-Z][A-Z0-9_]{2,})["']|environ\[\s*["']([A-Z][A-Z0-9_]{2,})["']"#,
        r#"|\$\{([A-Z][A-Z0-9_]{2,})(?::-[^}]*)?\}"#,
    ))
    .unwrap()
});
static HOST: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"https?://((?:[a-z0-9-]+\.)+[a-z]{2,}|localhost)(?::(\d{2,5}))?").unwrap());
/// Hosts that appear in comments, docs and boilerplate, not as integrations.
const NOISE_HOSTS: &[&str] = &[
    "github.com", "githubusercontent.com", "example.com", "example.org", "w3.org", "schema.org", "rubygems.org",
    "npmjs.com", "npmjs.org", "mozilla.org", "wikipedia.org", "creativecommons.org", "opensource.org",
    "apache.org", "json-schema.org", "semver.org", "yaml.org", "rubyonrails.org", "ruby-lang.org",
    "nodejs.org", "python.org", "golang.org", "rust-lang.org", "stackoverflow.com", "unpkg.com", "jsdelivr.net",
];

fn is_noise(host: &str) -> bool {
    NOISE_HOSTS.iter().any(|n| host == *n || host.ends_with(&format!(".{n}")))
}

/// A reference to another repository *as a service*, not just the word.
/// Repository names are often domain nouns (`company`, `candidate`, `api`)
/// that appear everywhere as model names, so a bare match proves nothing.
/// Counted: `company_api`, `COMPANY_URL`, `company-host`, `//company:3000`
/// (a compose service name) and `../company` or `company/app/` paths.
/// Hostnames such as `api.example.jp` are ambiguous and are reported under
/// "Hosts referenced" instead.
fn mention_regex(name: &str) -> Option<Regex> {
    let n = regex::escape(name);
    let pattern = [
        format!(r"(?i)\b{n}[_-](?:api|url|uri|host|endpoint|base[_-]?url|server|service|domain|origin|app|web|admin|client)\b"),
        format!(r"//{n}[:/]"),
        format!(r"\.\./{n}\b"),
        format!(r"\b{n}/(?:app|lib|src|config)/"),
    ]
    .join("|");
    let pattern = format!("(?i){}", pattern.replace("(?i)", ""));
    Regex::new(&pattern).ok()
}

/// Scans a repository's kept files for integration evidence.
pub fn signals(root: &Path, files: &[SourceFile], other_repos: &[String]) -> Signals {
    let mentions: Vec<(String, Regex)> = other_repos.iter().filter_map(|n| mention_regex(n).map(|re| (n.clone(), re))).collect();
    let mut out = Signals::default();
    for f in files.iter().filter(|f| !f.test) {
        let Ok(bytes) = fs::read(root.join(&f.path)) else { continue };
        let text = String::from_utf8_lossy(&bytes);
        for c in ENV_READ.captures_iter(&text) {
            if let Some(name) = c.iter().skip(1).flatten().next() {
                out.env.entry(name.as_str().to_string()).or_default().insert(f.path.clone());
            }
        }
        for c in HOST.captures_iter(&text) {
            let host = c[1].to_ascii_lowercase();
            if is_noise(&host) {
                continue;
            }
            let shown = match c.get(2) {
                Some(port) => format!("{host}:{}", port.as_str()),
                None => host,
            };
            out.hosts.entry(shown).or_default().insert(f.path.clone());
        }
        for (name, re) in &mentions {
            if re.is_match(&text) {
                out.mentions.entry(name.clone()).or_default().insert(f.path.clone());
            }
        }
    }
    out
}

fn top(map: &BTreeMap<String, BTreeSet<String>>) -> Vec<String> {
    let mut items: Vec<(&String, &BTreeSet<String>)> = map.iter().collect();
    items.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(b.0)));
    items
        .into_iter()
        .take(SIGNALS_PER_KIND)
        .map(|(k, files)| {
            let ex: Vec<&str> = files.iter().take(EXAMPLES_PER_SIGNAL).map(String::as_str).collect();
            format!("- {k} — {} file(s), e.g. {}", files.len(), ex.join(", "))
        })
        .collect()
}

fn interfaces(result: &RunResult) -> Vec<String> {
    let list = |v: Option<&Value>| -> String {
        v.and_then(Value::as_array)
            .map(|a| a.iter().take(5).map(|x| x.as_str().map(String::from).unwrap_or_else(|| x.to_string())).collect::<Vec<_>>().join("; "))
            .unwrap_or_default()
    };
    let rows: Vec<String> = if result.areas.is_empty() {
        result
            .summaries
            .iter()
            .filter(|s| !s.contains_key("error"))
            .map(|s| {
                format!(
                    "- {}: {} | exposes: {} | uses: {}",
                    str_of(s, "module"),
                    first_sentence(&str_of(s, "purpose")),
                    list(s.get("public_interface")),
                    list(s.get("depends_on"))
                )
            })
            .collect()
    } else {
        result
            .areas
            .iter()
            .map(|a| {
                format!(
                    "- {}: {} | exposes: {} | uses: {}",
                    str_of(a, "area"),
                    first_sentence(&str_of(a, "purpose")),
                    list(a.get("interfaces")),
                    list(a.get("depends_on"))
                )
            })
            .collect()
    };
    rows.into_iter().take(INTERFACE_MAX_LINES).collect()
}

/// Files next to the repositories that often describe how they run together
/// (a shared docker-compose, Makefile or README in the project folder).
pub fn workspace_files(parent: &Path) -> String {
    const NAMES: &[&str] = &["docker-compose.yml", "docker-compose.yaml", "compose.yml", "compose.yaml", "Makefile", "Procfile", "README.md"];
    let mut parts = Vec::new();
    let Ok(entries) = fs::read_dir(parent) else { return String::new() };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| NAMES.contains(&n.as_str()) || (n.starts_with("docker-compose") && (n.ends_with(".yml") || n.ends_with(".yaml"))))
        .collect();
    names.sort();
    names.dedup();
    for n in names {
        if let Ok(text) = fs::read_to_string(parent.join(&n)) {
            let body: String = text.chars().take(8_000).collect();
            parts.push(format!("=== {n} ===\n{body}"));
        }
    }
    parts.join("\n\n")
}

/// The prompt describing every repository, with its evidence.
pub fn system_input(repos: &[Repo]) -> String {
    system_input_with(repos, None)
}

/// As [`system_input`], plus shared files from the folder holding the repositories.
pub fn system_input_with(repos: &[Repo], parent: Option<&Path>) -> String {
    let names: Vec<String> = repos.iter().map(|r| r.name.clone()).collect();
    let mut blocks = Vec::new();
    for r in repos {
        let others: Vec<String> = names.iter().filter(|n| **n != r.name).cloned().collect();
        let sig = signals(&r.root, &r.result.plan.files, &others);
        let arch: String = r.result.documents.get("ARCHITECTURE.md").map(|t| t.chars().take(ARCHITECTURE_MAX_CHARS).collect()).unwrap_or_default();
        let section = |title: &str, lines: Vec<String>| {
            if lines.is_empty() { format!("{title}: none found") } else { format!("{title}:\n{}", lines.join("\n")) }
        };
        blocks.push(format!(
            "<repository name=\"{name}\" docs=\"{name}/README.md\">\n<architecture>\n{arch}\n</architecture>\n\n\
<components>\n{components}\n</components>\n\n<integration_evidence>\n{env}\n\n{hosts}\n\n{mentions}\n</integration_evidence>\n</repository>",
            name = r.name,
            components = interfaces(r.result).join("\n"),
            env = section("Environment variables read", top(&sig.env)),
            hosts = section("Hosts referenced", top(&sig.hosts)),
            mentions = section("References to the other repositories as services", top(&sig.mentions)),
        ));
    }
    if let Some(files) = parent.map(workspace_files).filter(|f| !f.is_empty()) {
        blocks.push(format!("<workspace_files note=\"files in the folder that holds the repositories\">\n{files}\n</workspace_files>"));
    }
    blocks.join("\n\n")
}

pub fn system_estimate(repos: usize) -> u64 {
    estimate_tokens((prompts::SYNTHESIS_SYSTEM.len() + repos * (ARCHITECTURE_MAX_CHARS + 12_000)) as u64)
}

/// Writes SYSTEM.md (from the cache when its inputs are unchanged).
/// Returns the document and whether it was reused.
pub fn system_doc(repos: &[Repo], config: &Config, store: &CacheStore, client: &dyn Llm) -> Result<(String, bool)> {
    system_doc_with(repos, None, config, store, client)
}

/// As [`system_doc`], also reading shared files from the parent folder.
pub fn system_doc_with(
    repos: &[Repo],
    parent: Option<&Path>,
    config: &Config,
    store: &CacheStore,
    client: &dyn Llm,
) -> Result<(String, bool)> {
    let user = prompts::system_user(&system_input_with(repos, parent), config.lang);
    let key = cache::key(&["system", &config.smart_model, &prompts::SYNTHESIS_SYSTEM, &user]);
    if let Some(text) = store.get(&key).and_then(|v| v.as_str().map(String::from)) {
        return Ok((text, true));
    }
    let text = strip_outer_fence(&client.complete(&config.smart_model, &prompts::SYNTHESIS_SYSTEM, &user, 8_000)?);
    store.put(key, Value::String(text.clone()));
    Ok((text, false))
}

/// The workspace README: links to SYSTEM.md and each repository's docs.
pub fn index_markdown(repos: &[Repo], config: &Config) -> String {
    let l = config.lang.labels();
    let rows: Vec<String> = repos
        .iter()
        .map(|r| {
            let at = r.commit.as_deref().map(|c| format!(" `{}`", &c[..c.len().min(12)])).unwrap_or_default();
            let src = r.url.clone().unwrap_or_else(|| r.name.clone());
            format!(
                "| [{name}]({name}/README.md) | {src}{at} | {files} | {modules} |",
                name = r.name,
                files = r.result.plan.files.len(),
                modules = r.result.plan.modules.len()
            )
        })
        .collect();
    format!(
        "# {title} — OwlMap\n\n| {doc} | {ans} |\n|---|---|\n| [{s0}](SYSTEM.md) | {s1} |\n\n## {repos}\n\n\
| {repo} | Source | {files} | {modules} |\n|---|---|---|---|\n{rows}\n\n---\n{footer}\n",
        title = l.workspace_title,
        doc = l.document,
        ans = l.answers,
        s0 = l.system.0,
        s1 = l.system.1,
        repos = l.repositories,
        repo = l.repository,
        files = l.files,
        modules = l.modules,
        rows = rows.join("\n"),
        footer = l.footer,
    )
}
