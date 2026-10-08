//! Walks a repository and keeps the files worth reading: source code, config
//! and docs. Skips dependencies, build output, binaries, lockfiles, secrets,
//! boilerplate and anything too large to be useful to a reader.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

/// Tests are summarised from their opening lines only.
pub const TEST_LINES: usize = 60;

#[derive(Clone, Debug, PartialEq)]
pub struct SourceFile {
    /// Relative path with `/` separators.
    pub path: String,
    pub size: u64,
    pub language: String,
    pub test: bool,
}

impl SourceFile {
    pub fn read(&self, root: &Path) -> String {
        let bytes = fs::read(root.join(&self.path)).unwrap_or_default();
        let text = String::from_utf8_lossy(&bytes).into_owned();
        if !self.test {
            return text;
        }
        let lines: Vec<&str> = text.split_inclusive('\n').collect();
        if lines.len() <= TEST_LINES {
            return text;
        }
        format!(
            "{}\n… ({} more lines of tests not shown)\n",
            lines[..TEST_LINES].concat(),
            lines.len() - TEST_LINES
        )
    }

    /// Characters this file contributes to a prompt (tests are truncated).
    pub fn prompt_size(&self) -> u64 {
        if self.test {
            self.size.min((TEST_LINES * 60) as u64)
        } else {
            self.size
        }
    }
}

#[derive(Debug, Default)]
pub struct ScanResult {
    pub files: Vec<SourceFile>,
    /// Reason → count, e.g. "lockfile" → 2.
    pub skipped: BTreeMap<String, usize>,
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
    ("ts", "TypeScript"), ("tsx", "TypeScript"), ("vue", "Vue"), ("svelte", "Svelte"),
    ("py", "Python"), ("go", "Go"), ("rs", "Rust"), ("java", "Java"), ("kt", "Kotlin"),
    ("swift", "Swift"), ("php", "PHP"), ("cs", "C#"), ("c", "C"), ("h", "C"), ("cpp", "C++"),
    ("hpp", "C++"), ("scala", "Scala"), ("ex", "Elixir"), ("exs", "Elixir"), ("dart", "Dart"),
    ("sql", "SQL"), ("graphql", "GraphQL"), ("proto", "Protobuf"),
    ("sh", "Shell"), ("yml", "YAML"), ("yaml", "YAML"), ("json", "JSON"), ("toml", "TOML"),
    ("md", "Markdown"), ("html", "HTML"), ("css", "CSS"), ("scss", "SCSS"),
];

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

pub struct Scanner {
    root: PathBuf,
    max_file_bytes: u64,
}

impl Scanner {
    pub fn new(root: impl Into<PathBuf>, max_file_bytes: u64) -> Self {
        Self { root: root.into(), max_file_bytes }
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
                if !(IGNORED_DIRS.contains(&name.as_str()) || IGNORED_DIRS.contains(&rel.as_str())) {
                    self.walk(&entry.path(), &rel, out);
                }
                continue;
            }
            if !meta.is_file() {
                continue;
            }
            match self.skip_reason(&entry.path(), &name, &rel, meta.len()) {
                Some(reason) => *out.skipped.entry(reason.into()).or_default() += 1,
                None => out.files.push(SourceFile {
                    language: language_for(&rel).unwrap_or("").to_string(),
                    test: is_test_path(&rel),
                    path: rel,
                    size: meta.len(),
                }),
            }
        }
    }

    fn skip_reason(&self, abs: &Path, base: &str, rel: &str, size: u64) -> Option<&'static str> {
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
        if size > self.max_file_bytes {
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
