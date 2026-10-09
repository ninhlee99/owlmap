//! Resolves the user's input to a directory on disk.
//! - A public GitHub URL is shallow-cloned into a temp dir, deleted on drop.
//! - A local path is used in place and never modified or removed.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;

use anyhow::{bail, Result};
use regex::Regex;
use tempfile::TempDir;

pub static GITHUB_URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^https://github\.com/(?P<owner>[A-Za-z0-9][A-Za-z0-9-]{0,38})/(?P<repo>[A-Za-z0-9._-]{1,100}?)(?:\.git)?/?$").unwrap()
});

pub struct RepoSource {
    pub path: PathBuf,
    pub name: String,
    pub url: Option<String>,
    pub commit: Option<String>,
    _tmp: Option<TempDir>,
}

impl RepoSource {
    pub fn resolve(input: &str) -> Result<Self> {
        let input = input.trim();
        if let Some(c) = GITHUB_URL.captures(input) {
            let (owner, repo) = (&c["owner"], &c["repo"]);
            let url = format!("https://github.com/{owner}/{repo}");
            let tmp = tempfile::Builder::new().prefix("owlmap-").tempdir()?;
            let path = tmp.path().join("repo");
            // Arguments go straight to git, never through a shell; never prompt for credentials.
            let out = Command::new("git")
                .env("GIT_TERMINAL_PROMPT", "0")
                .args(["-c", "core.symlinks=false", "clone", "--depth", "1", "--quiet", "--"])
                .arg(&url)
                .arg(&path)
                .output()?;
            if !out.status.success() {
                let err = String::from_utf8_lossy(&out.stderr);
                bail!("Could not clone {url}. Is it a public repository? ({})", err.trim().lines().last().unwrap_or(""));
            }
            return Ok(Self { commit: git_head(&path), path, name: format!("{owner}-{repo}"), url: Some(url), _tmp: Some(tmp) });
        }

        let p = Path::new(input);
        if p.is_dir() {
            let path = p.canonicalize()?;
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "repo".into());
            return Ok(Self { commit: git_head(&path), path, name, url: None, _tmp: None });
        }
        bail!("Not a public GitHub URL (https://github.com/owner/repo) or a local folder: {input}")
    }
}

fn git_head(dir: &Path) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(dir).args(["rev-parse", "HEAD"]).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Folders never treated as repositories when looking inside a parent folder.
const NOT_REPOS: &[&str] = &["node_modules", "vendor", "owlmap", "owlmap-docs", "tmp", "target", "dist", "build"];

/// If `dir` is not itself a git repository but holds git repositories one level
/// down (a "project" folder such as `candidate/ company/ api/`), returns them
/// sorted by name. Returns `None` for a repository or a plain folder.
pub fn discover(dir: &Path) -> Option<Vec<PathBuf>> {
    if !dir.is_dir() || is_repo(dir) {
        return None;
    }
    let mut repos: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            !name.starts_with('.') && !NOT_REPOS.contains(&name.as_str())
        })
        .map(|e| e.path())
        .filter(|p| is_repo(p))
        .collect();
    repos.sort();
    (!repos.is_empty()).then_some(repos)
}

/// A `.git` folder, or a `.git` file (worktrees and submodules).
fn is_repo(dir: &Path) -> bool {
    dir.join(".git").exists()
}
