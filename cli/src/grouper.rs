//! Groups files into "modules" — the units Claude summarises one at a time.
//!
//! 1. A module is usually a directory. Conventional containers (app/, src/,
//!    lib/…) are split one level deeper, so app/models and app/controllers
//!    become their own modules.
//! 2. A module larger than the character budget is split by the next directory
//!    level; a single directory that is still too large is cut into parts.
//! 3. Tiny modules are folded into their parent folder, and tiny top-level
//!    folders are batched together, so no API call is spent on two files.
//! 4. Tests are kept apart from the code they test, one module per top-level
//!    area, and are read in truncated form (see `SourceFile::read`).

use crate::scanner::SourceFile;

pub const ROOT: &str = "(root)";
pub const SMALL_FOLDERS: &str = "(small folders)";
const CONTAINERS: &[&str] = &[
    "app", "src", "lib", "packages", "apps", "internal", "pkg", "cmd", "services", "modules", "components", "features",
];
const TEST_ROOTS: &[&str] = &["test", "tests", "spec", "specs", "__tests__", "e2e", "cypress"];

#[derive(Clone, Debug)]
pub struct Module {
    pub name: String,
    pub files: Vec<SourceFile>,
}

impl Module {
    pub fn bytes(&self) -> u64 {
        self.files.iter().map(SourceFile::prompt_size).sum()
    }

    pub fn slug(&self) -> String {
        slug(&self.name)
    }
}

pub fn slug(name: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in name.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            dash = false;
        } else if !dash {
            out.push('-');
            dash = true;
        }
    }
    let s = out.trim_matches('-').to_string();
    if s.is_empty() { "root".into() } else { s }
}

pub struct Grouper {
    budget: u64,
    min: u64,
}

impl Grouper {
    pub fn new(char_budget: u64, min_chars: u64) -> Self {
        Self { budget: char_budget, min: min_chars }
    }

    pub fn group(&self, files: &[SourceFile]) -> Vec<Module> {
        let (tests, code): (Vec<_>, Vec<_>) = files.iter().cloned().partition(|f| f.test);
        let mut modules: Vec<Module> = group_ordered(code, |f| base_key(&f.path))
            .into_iter()
            .flat_map(|(key, fs)| {
                let depth = depth_of(&key);
                self.split(&key, fs, depth)
            })
            .collect();
        modules = self.fold_small(modules);
        for (area, fs) in group_ordered(tests, |f| top_area(&f.path)) {
            modules.extend(self.parts(&format!("{area} (tests)"), fs));
        }
        modules.sort_by(|a, b| a.name.cmp(&b.name));
        modules
    }

    fn split(&self, key: &str, files: Vec<SourceFile>, depth: usize) -> Vec<Module> {
        let module = Module { name: key.to_string(), files };
        if module.bytes() <= self.budget {
            return vec![module];
        }
        let files = module.files;
        let deeper = group_ordered(files.clone(), |f| key_at(&f.path, depth + 1));
        if deeper.len() > 1 {
            return deeper.into_iter().flat_map(|(k, fs)| self.split(&k, fs, depth + 1)).collect();
        }
        // Everything sits in one deeper folder (e.g. lib/foo/bar/*): keep descending.
        let only = deeper[0].0.clone();
        if only != key {
            return self.split(&only, files, depth + 1);
        }
        self.parts(key, files)
    }

    fn parts(&self, key: &str, files: Vec<SourceFile>) -> Vec<Module> {
        let mut chunks: Vec<Vec<SourceFile>> = Vec::new();
        let mut current = Vec::new();
        let mut size = 0;
        for f in files {
            if size + f.prompt_size() > self.budget && !current.is_empty() {
                chunks.push(std::mem::take(&mut current));
                size = 0;
            }
            size += f.prompt_size();
            current.push(f);
        }
        if !current.is_empty() {
            chunks.push(current);
        }
        if chunks.len() == 1 {
            return vec![Module { name: key.to_string(), files: chunks.pop().unwrap() }];
        }
        chunks
            .into_iter()
            .enumerate()
            .map(|(i, fs)| Module { name: format!("{key} (part {})", i + 1), files: fs })
            .collect()
    }

    /// Fold tiny nested modules into their parent folder (when that keeps the
    /// parent within budget), then batch tiny top-level folders together.
    fn fold_small(&self, mut modules: Vec<Module>) -> Vec<Module> {
        loop {
            let pick = modules.iter().position(|m| {
                if !(m.bytes() < self.min && m.name.contains('/') && !m.name.contains("(part")) {
                    return false;
                }
                match modules.iter().find(|p| p.name == parent_of(&m.name)) {
                    None => true,
                    Some(p) => p.bytes() + m.bytes() <= self.budget,
                }
            });
            let Some(i) = pick else { break };
            let small = modules.remove(i);
            let parent_name = parent_of(&small.name);
            let mut files = match modules.iter().position(|p| p.name == parent_name) {
                Some(j) => modules.remove(j).files,
                None => Vec::new(),
            };
            files.extend(small.files);
            modules.push(Module { name: parent_name, files });
        }

        let (tiny, rest): (Vec<_>, Vec<_>) = modules
            .into_iter()
            .partition(|m| m.bytes() < self.min && !m.name.contains('/') && m.name != ROOT);
        if tiny.len() < 2 {
            return rest.into_iter().chain(tiny).collect();
        }
        let mut out = rest;
        out.extend(self.parts(SMALL_FOLDERS, tiny.into_iter().flat_map(|m| m.files).collect()));
        out
    }
}

/// Group while keeping first-appearance order (files arrive sorted by path).
fn group_ordered(files: Vec<SourceFile>, key: impl Fn(&SourceFile) -> String) -> Vec<(String, Vec<SourceFile>)> {
    let mut groups: Vec<(String, Vec<SourceFile>)> = Vec::new();
    for f in files {
        let k = key(&f);
        match groups.iter_mut().find(|(g, _)| *g == k) {
            Some((_, v)) => v.push(f),
            None => groups.push((k, vec![f])),
        }
    }
    groups
}

fn dir_parts(path: &str) -> Vec<&str> {
    let mut parts: Vec<&str> = path.split('/').collect();
    parts.pop(); // file name
    parts
}

fn base_key(path: &str) -> String {
    let parts = dir_parts(path);
    match parts.as_slice() {
        [] => ROOT.into(),
        [first, ..] if CONTAINERS.contains(first) && parts.len() >= 2 => parts[..2].join("/"),
        [first, ..] => (*first).into(),
    }
}

fn key_at(path: &str, depth: usize) -> String {
    let parts = dir_parts(path);
    if parts.is_empty() {
        return ROOT.into();
    }
    parts[..depth.min(parts.len())].join("/")
}

fn depth_of(key: &str) -> usize {
    if key == ROOT { 0 } else { key.matches('/').count() + 1 }
}

fn parent_of(name: &str) -> String {
    match name.rfind('/') {
        Some(i) => name[..i].to_string(),
        None => ".".into(),
    }
}

fn top_area(path: &str) -> String {
    let first = path.split('/').next().unwrap_or(path);
    if !path.contains('/') || TEST_ROOTS.iter().any(|t| t.eq_ignore_ascii_case(first)) {
        "project".into()
    } else {
        first.into()
    }
}
