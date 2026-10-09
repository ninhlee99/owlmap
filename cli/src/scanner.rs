//! Walks a repository and keeps the files worth reading: source code, config
//! and docs. Skips dependencies, build output, binaries, lockfiles, secrets,
//! boilerplate, low-signal bulk (migrations, translations, fixtures, generated
//! code) and anything too large to be useful.
//!
//! Large files are not read in full: tests and data files are read from their
//! first lines, and big source files as an outline (head + every declaration).

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use anyhow::{Context, Result};
use globset::{Glob, GlobSet, GlobSetBuilder};
use regex::Regex;

/// How much of each file to read. Documentation needs the shape of the code
/// more than every line, so `Standard` reads long files as outlines.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum Detail {
    /// Outlines for almost everything; test names only. Cheapest.
    Quick,
    /// Full short files, outlines for long ones, test names only.
    #[default]
    Standard,
    /// Full files up to 400 lines; tests from their first 60 lines.
    Deep,
}

impl Detail {
    fn limits(self) -> Limits {
        match self {
            Detail::Quick => Limits { outline_min_lines: 40, outline_head: 15, test_lines: 8, data_lines: 30 },
            Detail::Standard => Limits { outline_min_lines: 150, outline_head: 40, test_lines: 12, data_lines: 60 },
            Detail::Deep => Limits { outline_min_lines: 400, outline_head: 120, test_lines: 60, data_lines: 60 },
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Limits {
    outline_min_lines: u16,
    outline_head: u16,
    test_lines: u16,
    data_lines: u16,
}

/// Bytes per line, for estimates and for deciding when a file is "long".
const BYTES_PER_LINE: u64 = 40;
const OUTLINE_MAX_SIGNATURES: usize = 250;
/// Data and markup files at or below this size are read in full.
const DATA_FULL_MAX_BYTES: u64 = 8_000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReadMode {
    #[default]
    Full,
    /// The first N lines.
    Head(u16),
    /// Files longer than `min_lines`: the first `head` lines plus every declaration.
    Outline { head: u16, min_lines: u16 },
    /// Test names (describe/it/test…), at most N; the first N lines if none match.
    TestNames(u16),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SourceFile {
    /// Relative path with `/` separators.
    pub path: String,
    pub size: u64,
    pub language: String,
    pub test: bool,
    pub mode: ReadMode,
}

impl SourceFile {
    pub fn read(&self, root: &Path) -> String {
        let bytes = fs::read(root.join(&self.path)).unwrap_or_default();
        let text = String::from_utf8_lossy(&bytes).into_owned();
        match self.mode {
            ReadMode::Full => text,
            ReadMode::Head(n) => head(&text, n as usize),
            ReadMode::Outline { head, min_lines } => outline(&text, head as usize, min_lines as usize),
            ReadMode::TestNames(n) => test_names(&text, n as usize),
        }
    }

    /// Approximate characters this file contributes to a prompt.
    pub fn prompt_size(&self) -> u64 {
        let lines = |n: u16| u64::from(n) * BYTES_PER_LINE;
        match self.mode {
            ReadMode::Full => self.size,
            ReadMode::Head(n) | ReadMode::TestNames(n) => self.size.min(lines(n)),
            ReadMode::Outline { head, min_lines } => {
                if self.size <= lines(min_lines) { self.size } else { lines(head) + (self.size - lines(head)) / 10 }
            }
        }
    }
}

fn head(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    if lines.len() <= n {
        return text.to_string();
    }
    format!("{}\n… ({} more lines not shown)\n", lines[..n].concat(), lines.len() - n)
}

static TEST_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^\s*(?:RSpec\.|test\.|t\.)?(?:describe|context|it|test|specify|scenario|feature|example|module|acceptance|shared_examples)\b[\s(.]|^\s*(?:async\s+)?(?:def|func|fn)\s+test|^\s*#\[test\]|^\s*@Test|^\s*class\s+\w*Test"#).unwrap()
});

fn test_names(text: &str, n: usize) -> String {
    let names: Vec<String> = text
        .lines()
        .filter(|l| TEST_NAME.is_match(l))
        .take(n + 1)
        .map(|l| l.trim_end().chars().take(140).collect())
        .collect();
    if names.is_empty() {
        return head(text, n);
    }
    let more = if names.len() > n { "\n…" } else { "" };
    format!("{}{more}\n", names[..names.len().min(n)].join("\n"))
}

/// Declarations across common languages: classes, modules, functions, methods,
/// routes, exports, types. Good enough to see a file's shape without reading it all.
static SIGNATURE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^\s*(?:(?:pub(?:\([^)]*\))?|export(?:\s+default)?|public|private|protected|internal|static|abstract|final|async|override|open)\s+)*(?:def|class|module|struct|enum|trait|impl|interface|type|fn|func|function|const\s+\w+\s*=\s*(?:async\s*)?\(|scope|has_many|has_one|belongs_to|validates|before_action|after_action|resources?|get|post|put|patch|delete|namespace|router\.\w+|app\.(?:get|post|put|patch|delete|use))\b",
    )
    .unwrap()
});

fn outline(text: &str, head_lines: usize, min_lines: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= min_lines {
        return text.to_string();
    }
    let mut out = lines[..head_lines.min(lines.len())].join("\n");
    out.push_str(&format!("\n… ({} more lines; declarations below with line numbers)\n", lines.len() - head_lines));
    let mut shown = 0;
    for (i, line) in lines.iter().enumerate().skip(head_lines) {
        if SIGNATURE.is_match(line) {
            if shown == OUTLINE_MAX_SIGNATURES {
                out.push_str("… (more declarations not shown)\n");
                break;
            }
            let trimmed: String = line.trim_end().chars().take(160).collect();
            out.push_str(&format!("{:>6}: {trimmed}\n", i + 1));
            shown += 1;
        }
    }
    out
}

#[derive(Debug, Default)]
pub struct ScanResult {
    pub files: Vec<SourceFile>,
    /// Reason → count, e.g. "lockfile" → 2.
    pub skipped: BTreeMap<String, usize>,
}

/// What to keep. The defaults suit almost every repository.
#[derive(Clone, Debug)]
pub struct ScanOptions {
    pub max_file_bytes: u64,
    pub detail: Detail,
    /// Drop test files entirely instead of reading their first lines.
    pub skip_tests: bool,
    /// Keep migrations, translations, fixtures and generated code.
    pub all_files: bool,
    /// When non-empty, only paths matching one of these globs are kept.
    pub include: Vec<String>,
    /// Paths matching any of these globs are dropped.
    pub exclude: Vec<String>,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self { max_file_bytes: 100_000, detail: Detail::Standard, skip_tests: false, all_files: false, include: vec![], exclude: vec![] }
    }
}

const IGNORED_DIRS: &[&str] = &[
    ".git", ".hg", ".svn", "node_modules", "vendor", "bower_components", ".bundle", "dist", "build", "out",
    "target", "bin", "obj", ".next", ".nuxt", ".svelte-kit", ".turbo", ".cache", "coverage", "tmp", "log",
    "logs", "public/assets", "public/packs", "storage", "__pycache__", ".venv", "venv", "env", ".tox",
    ".mypy_cache", ".pytest_cache", ".idea", ".vscode", ".gradle", "Pods", "DerivedData",
];

const IGNORED_FILES: &[&str] = &[
    "package-lock.json", "yarn.lock", "pnpm-lock.yaml", "bun.lockb", "Gemfile.lock", "Cargo.lock",
    "composer.lock", "poetry.lock", "Pipfile.lock", "go.sum", "mix.lock", "pubspec.lock", ".DS_Store",
];

/// Extension-less files that still describe the project.
const NAMED_FILES: &[&str] = &["Gemfile", "Rakefile", "Dockerfile", "Makefile", "Procfile", "Brewfile", "Podfile"];

const LANGUAGES: &[(&str, &str)] = &[
    ("rb", "Ruby"), ("rake", "Ruby"), ("erb", "ERB"), ("haml", "Haml"), ("slim", "Slim"),
    ("js", "JavaScript"), ("jsx", "JavaScript"), ("mjs", "JavaScript"), ("cjs", "JavaScript"),
    ("ts", "TypeScript"), ("tsx", "TypeScript"), ("vue", "Vue"), ("svelte", "Svelte"), ("gjs", "JavaScript"),
    ("py", "Python"), ("go", "Go"), ("rs", "Rust"), ("java", "Java"), ("kt", "Kotlin"),
    ("swift", "Swift"), ("php", "PHP"), ("cs", "C#"), ("c", "C"), ("h", "C"), ("cpp", "C++"),
    ("hpp", "C++"), ("scala", "Scala"), ("ex", "Elixir"), ("exs", "Elixir"), ("dart", "Dart"),
    ("sql", "SQL"), ("graphql", "GraphQL"), ("proto", "Protobuf"),
    ("sh", "Shell"), ("yml", "YAML"), ("yaml", "YAML"), ("json", "JSON"), ("toml", "TOML"),
    ("md", "Markdown"), ("html", "HTML"), ("css", "CSS"), ("scss", "SCSS"),
];

/// Languages whose large files are data or markup rather than logic.
const DATA_LANGUAGES: &[&str] = &["YAML", "JSON", "TOML", "SQL", "Markdown", "HTML", "CSS", "SCSS", "GraphQL"];

/// Never read anything that commonly holds credentials.
static SECRET: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [r"^\.env(\..*)?$", r"\.pem$", r"\.key$", r"^id_(rsa|ed25519)", r"(?i)credentials", r"\.p12$"]
        .iter()
        .map(|p| Regex::new(p).unwrap())
        .collect()
});
static BOILERPLATE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(CHANGELOG|CHANGES|HISTORY|NEWS|LICENSE|LICENCE|COPYING|CODE_OF_CONDUCT|CONTRIBUTING|SECURITY|AUTHORS|CONTRIBUTORS)(\.[a-z]+)?$").unwrap()
});
static TEST_PATH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(^|/)(test|tests|spec|specs|__tests__|e2e|cypress)/|(_test|_spec|\.test|\.spec)\.[a-z]+$").unwrap()
});
static MINIFIED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\.min\.(js|css)$").unwrap());
/// Bulk that is big but says little about how the system works.
static LOW_SIGNAL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?i)(^|/)db/migrate/|(^|/)migrations?/",
        r"|(^|/)(locales?|i18n|translations)/.*\.(ya?ml|json|po|pot|properties)$",
        r"|(^|/)(fixtures?|__fixtures__|__snapshots__|snapshots)/|\.snap$",
        r"|(^|/)(generated|__generated__|gen)/|\.pb\.go$|_pb2\.py$|\.generated\.\w+$",
        r"|(^|/)public/|(^|/)static/vendor/",
    ))
    .unwrap()
});

pub fn is_test_path(path: &str) -> bool {
    TEST_PATH.is_match(path)
}

pub fn language_for(path: &str) -> Option<&'static str> {
    let base = path.rsplit('/').next().unwrap_or(path);
    if NAMED_FILES.contains(&base) {
        return Some("Build/config");
    }
    let ext = Path::new(base).extension()?.to_str()?.to_ascii_lowercase();
    LANGUAGES.iter().find(|(e, _)| *e == ext).map(|(_, l)| *l)
}

fn glob_set(patterns: &[String]) -> Result<Option<GlobSet>> {
    if patterns.is_empty() {
        return Ok(None);
    }
    let mut b = GlobSetBuilder::new();
    for p in patterns {
        // "app" or "app/" means everything under it.
        let p = p.trim_end_matches('/');
        b.add(Glob::new(p).with_context(|| format!("bad glob: {p}"))?);
        if !p.contains('*') {
            b.add(Glob::new(&format!("{p}/**"))?);
        }
    }
    Ok(Some(b.build()?))
}

pub struct Scanner {
    root: PathBuf,
    opts: ScanOptions,
    include: Option<GlobSet>,
    exclude: Option<GlobSet>,
}

impl Scanner {
    pub fn new(root: impl Into<PathBuf>, max_file_bytes: u64) -> Self {
        Self::with_options(root, ScanOptions { max_file_bytes, ..Default::default() }).expect("default options are valid")
    }

    pub fn with_options(root: impl Into<PathBuf>, opts: ScanOptions) -> Result<Self> {
        Ok(Self { include: glob_set(&opts.include)?, exclude: glob_set(&opts.exclude)?, root: root.into(), opts })
    }

    pub fn scan(&self) -> ScanResult {
        let mut result = ScanResult::default();
        self.walk(&self.root.clone(), "", &mut result);
        result.files.sort_by(|a, b| a.path.cmp(&b.path));
        result
    }

    fn walk(&self, dir: &Path, rel_dir: &str, out: &mut ScanResult) {
        let Ok(entries) = fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let rel = if rel_dir.is_empty() { name.clone() } else { format!("{rel_dir}/{name}") };
            // symlink_metadata: never follow links, in or out of the repository.
            let Ok(meta) = fs::symlink_metadata(entry.path()) else { continue };
            if meta.file_type().is_symlink() {
                *out.skipped.entry("symlink".into()).or_default() += 1;
                continue;
            }
            if meta.is_dir() {
                let ignored = IGNORED_DIRS.contains(&name.as_str()) || IGNORED_DIRS.contains(&rel.as_str());
                let excluded = self.exclude.as_ref().is_some_and(|g| g.is_match(&rel));
                if !ignored && !excluded {
                    self.walk(&entry.path(), &rel, out);
                } else if excluded {
                    *out.skipped.entry("excluded".into()).or_default() += 1;
                }
                continue;
            }
            if !meta.is_file() {
                continue;
            }
            match self.skip_reason(&entry.path(), &name, &rel, meta.len()) {
                Some(reason) => *out.skipped.entry(reason.into()).or_default() += 1,
                None => {
                    let language = language_for(&rel).unwrap_or("");
                    let test = is_test_path(&rel);
                    let l = self.opts.detail.limits();
                    let mode = if test {
                        if self.opts.detail == Detail::Deep { ReadMode::Head(l.test_lines) } else { ReadMode::TestNames(l.test_lines) }
                    } else if DATA_LANGUAGES.contains(&language) {
                        if meta.len() > DATA_FULL_MAX_BYTES { ReadMode::Head(l.data_lines) } else { ReadMode::Full }
                    } else if meta.len() > u64::from(l.outline_min_lines) * BYTES_PER_LINE {
                        ReadMode::Outline { head: l.outline_head, min_lines: l.outline_min_lines }
                    } else {
                        ReadMode::Full
                    };
                    out.files.push(SourceFile { language: language.to_string(), test, mode, path: rel, size: meta.len() });
                }
            }
        }
    }

    fn skip_reason(&self, abs: &Path, base: &str, rel: &str, size: u64) -> Option<&'static str> {
        if self.exclude.as_ref().is_some_and(|g| g.is_match(rel)) {
            return Some("excluded");
        }
        if self.include.as_ref().is_some_and(|g| !g.is_match(rel)) {
            return Some("not_included");
        }
        if IGNORED_FILES.contains(&base) {
            return Some("lockfile");
        }
        if BOILERPLATE.is_match(base) {
            return Some("boilerplate");
        }
        if SECRET.iter().any(|re| re.is_match(base)) {
            return Some("secret");
        }
        if MINIFIED.is_match(base) {
            return Some("minified");
        }
        if language_for(rel).is_none() {
            return Some("unsupported");
        }
        if !self.opts.all_files && LOW_SIGNAL.is_match(rel) {
            return Some("low_signal");
        }
        if self.opts.skip_tests && is_test_path(rel) {
            return Some("test");
        }
        if size > self.opts.max_file_bytes {
            return Some("too_large");
        }
        if size == 0 {
            return Some("empty");
        }
        if is_binary(abs) {
            return Some("binary");
        }
        None
    }
}

fn is_binary(path: &Path) -> bool {
    let mut buf = [0u8; 4096];
    match fs::File::open(path).and_then(|mut f| f.read(&mut buf)) {
        Ok(n) => buf[..n].contains(&0),
        Err(_) => true,
    }
}
