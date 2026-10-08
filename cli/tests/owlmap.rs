//! Offline tests: a fake Claude stands in for the API, and a stub `claude`
//! executable stands in for Claude Code, so nothing here costs tokens.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{bail, Result};
use owlmap::analyzer::{parse_json_object, Analyzer};
use owlmap::client::{ClaudeCodeClient, Llm, Usage};
use owlmap::config::Config;
use owlmap::grouper::{slug, Grouper};
use owlmap::prompts;
use owlmap::repo_source::{RepoSource, GITHUB_URL};
use owlmap::scanner::{Scanner, SourceFile};
use owlmap::writer::{self, SourceInfo};
use serde_json::json;

// ---- helpers ---------------------------------------------------------------

fn make_repo(files: &[(&str, &[u8])]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (path, body) in files {
        let full = dir.path().join(path);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(full, body).unwrap();
    }
    dir
}

struct Call {
    model: String,
    system: String,
    user: String,
}

/// Module calls get JSON, overview calls get Markdown.
#[derive(Default)]
struct FakeClient {
    module_reply: Option<String>,
    fail_on: Option<String>,
    calls: Mutex<Vec<Call>>,
}

impl Llm for FakeClient {
    fn complete(&self, model: &str, system: &str, user: &str, _max: u32) -> Result<String> {
        self.calls.lock().unwrap().push(Call { model: model.into(), system: system.into(), user: user.into() });
        if let Some(f) = &self.fail_on {
            if user.contains(f.as_str()) {
                bail!("boom");
            }
        }
        if system == prompts::MODULE_SYSTEM.as_str() {
            return Ok(self.module_reply.clone().unwrap_or_else(|| {
                "```json\n{\"purpose\":\"Handles things. More detail.\",\"key_files\":[{\"path\":\"a.rb\",\"role\":\"main\"}],\
\"public_interface\":[],\"depends_on\":[\"Redis\"],\"used_by\":[],\"data\":[],\"risks\":[\"Tax rates are duplicated\"],\"notes\":\"\"}\n```"
                    .into()
            }));
        }
        let doc = user.split("Write ").nth(1).and_then(|s| s.split(".md").next()).unwrap_or("?");
        Ok(format!("```markdown\n# Doc for {doc}\n\nBody.\n```"))
    }

    fn usage(&self) -> Usage {
        Usage::default()
    }
}

fn sf(path: &str, size: u64) -> SourceFile {
    SourceFile { path: path.into(), size, language: "Ruby".into(), test: false }
}

fn names(mods: &[owlmap::grouper::Module]) -> Vec<&str> {
    mods.iter().map(|m| m.name.as_str()).collect()
}

fn quiet_config() -> Config {
    Config { concurrency: 2, min_module_chars: 0, ..Config::default() }
}

fn shop() -> tempfile::TempDir {
    make_repo(&[
        ("README.md", b"# Shop\nRun with bin/dev\n"),
        ("Gemfile", b"gem 'rails'\n"),
        ("app/models/user.rb", b"class User; end\n"),
        ("app/services/auth_service.rb", b"class AuthService; end\n"),
        ("config/routes.rb", b"Rails.application.routes.draw {}\n"),
    ])
}

// ---- scanner ---------------------------------------------------------------

#[test]
fn scanner_keeps_source_and_skips_noise() {
    let huge = vec![b'x'; 200_000];
    let root = make_repo(&[
        ("app/models/user.rb", b"class User; end\n"),
        ("README.md", b"# Hi\n"),
        ("Gemfile", b"source 'https://rubygems.org'\n"),
        ("Gemfile.lock", b"GEM\n"),
        ("node_modules/x/index.js", b"x\n"),
        (".env", b"SECRET=1\n"),
        ("config/master.key", b"abc\n"),
        ("public/logo.png", b"\x89PNG\x00\x00"),
        ("assets/app.min.js", b"a\n"),
        ("lib/blob.rb", b"a\x00b"),
        ("lib/huge.rb", &huge),
        ("lib/empty.rb", b""),
        ("CHANGELOG.md", b"v1\n"),
        ("spec/user_spec.rb", b"describe User\n"),
    ]);
    let r = Scanner::new(root.path(), 100_000).scan();
    let paths: Vec<_> = r.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, ["Gemfile", "README.md", "app/models/user.rb", "spec/user_spec.rb"]);
    assert_eq!(r.files.iter().map(|f| f.test).collect::<Vec<_>>(), [false, false, false, true]);
    for (reason, n) in [("lockfile", 1), ("secret", 2), ("too_large", 1), ("binary", 1), ("minified", 1), ("boilerplate", 1)] {
        assert_eq!(r.skipped.get(reason).copied().unwrap_or(0), n, "{reason}");
    }
}

#[cfg(unix)]
#[test]
fn scanner_does_not_follow_symlinks() {
    let root = make_repo(&[("lib/a.rb", b"x\n")]);
    let outside = make_repo(&[("secret.rb", b"TOP SECRET\n")]);
    std::os::unix::fs::symlink(outside.path(), root.path().join("lib/linked")).unwrap();
    std::os::unix::fs::symlink(outside.path().join("secret.rb"), root.path().join("lib/s.rb")).unwrap();
    let paths: Vec<_> = Scanner::new(root.path(), 100_000).scan().files.into_iter().map(|f| f.path).collect();
    assert_eq!(paths, ["lib/a.rb"]);
}

#[test]
fn test_files_are_truncated_when_read() {
    let body: String = (0..100).map(|i| format!("line {i}\n")).collect();
    let root = make_repo(&[("spec/big_spec.rb", body.as_bytes())]);
    let f = SourceFile { path: "spec/big_spec.rb".into(), size: body.len() as u64, language: "Ruby".into(), test: true };
    let text = f.read(root.path());
    assert!(text.contains("line 59\n") && !text.contains("line 60\n"));
    assert!(text.contains("40 more lines of tests not shown"));
}

// ---- grouper ---------------------------------------------------------------

#[test]
fn grouper_splits_containers_one_level_deeper() {
    let files = [sf("Gemfile", 5), sf("app/controllers/a.rb", 10), sf("app/models/user.rb", 10), sf("config/routes.rb", 10)];
    let mods = Grouper::new(1_000, 0).group(&files);
    assert_eq!(names(&mods), ["(root)", "app/controllers", "app/models", "config"]);
}

#[test]
fn grouper_splits_oversized_by_subfolder_then_parts() {
    let files = [sf("lib/a/one.rb", 600), sf("lib/a/two.rb", 600), sf("lib/b/three.rb", 300)];
    let mods = Grouper::new(1_000, 0).group(&files);
    assert_eq!(names(&mods), ["lib/a (part 1)", "lib/a (part 2)", "lib/b"]);
}

#[test]
fn grouper_descends_through_single_child_folders() {
    let files = [sf("lib/deep/x/a.rb", 600), sf("lib/deep/y/b.rb", 600)];
    let mods = Grouper::new(1_000, 0).group(&files);
    assert_eq!(names(&mods), ["lib/deep/x", "lib/deep/y"]);
}

#[test]
fn grouper_folds_tiny_modules() {
    let files = [
        sf("app/models/concerns/x.rb", 100),
        sf("app/models/user.rb", 5_000),
        sf("config/c.rb", 5_000),
        sf("docs/a.md", 100),
        sf("script/b.sh", 100),
    ];
    let mods = Grouper::new(50_000, 3_000).group(&files);
    assert_eq!(names(&mods), ["(small folders)", "app/models", "config"]);
    let models = mods.iter().find(|m| m.name == "app/models").unwrap();
    assert!(models.files.iter().any(|f| f.path == "app/models/concerns/x.rb"));
}

#[test]
fn grouper_keeps_tests_apart() {
    let files = [
        SourceFile { test: true, ..sf("contrib/spec/b_spec.rb", 100) },
        sf("lib/a.rb", 5_000),
        SourceFile { test: true, ..sf("test/a_test.rb", 90_000) },
    ];
    let mods = Grouper::new(50_000, 3_000).group(&files);
    assert_eq!(names(&mods), ["contrib (tests)", "lib", "project (tests)"]);
    assert_eq!(mods[2].bytes(), 3_600, "test files count at their truncated size");
}

#[test]
fn slugs() {
    assert_eq!(slug("app/models (part 2)"), "app-models-part-2");
    assert_eq!(slug("(root)"), "root");
}

// ---- repo source -----------------------------------------------------------

#[test]
fn repo_source_rejects_non_github_and_odd_urls() {
    for input in [
        "http://github.com/a/b",
        "https://gitlab.com/a/b",
        "https://github.com/a",
        "https://github.com/a/b;rm -rf",
        "https://github.com/-x/b",
        "/definitely/not/here",
    ] {
        assert!(RepoSource::resolve(input).is_err(), "{input}");
    }
}

#[test]
fn repo_source_accepts_github_url_forms() {
    for u in ["https://github.com/rails/rails", "https://github.com/rails/rails/", "https://github.com/rails/rails.git"] {
        let c = GITHUB_URL.captures(u).unwrap_or_else(|| panic!("{u}"));
        assert_eq!(&c["repo"], "rails");
    }
}

#[test]
fn repo_source_uses_local_folder_in_place() {
    let dir = tempfile::tempdir().unwrap();
    {
        let src = RepoSource::resolve(dir.path().to_str().unwrap()).unwrap();
        assert_eq!(src.path, dir.path().canonicalize().unwrap());
    }
    assert!(dir.path().is_dir(), "dropping the source must never delete a local folder");
}

// ---- analyzer --------------------------------------------------------------

#[test]
fn analyzer_end_to_end_with_fake_client() {
    let root = shop();
    let config = quiet_config();
    let client = FakeClient::default();
    let r = Analyzer::new(root.path(), "shop", &config).quiet().run(&client).unwrap();

    assert_eq!(r.summaries.len(), 4);
    assert!(r.failures.is_empty());
    assert_eq!(r.summaries[0]["purpose"], "Handles things. More detail.");
    assert_eq!(r.documents.keys().collect::<Vec<_>>(), ["ARCHITECTURE.md", "FLOWS.md", "ONBOARDING.md"]);
    assert!(r.documents["FLOWS.md"].starts_with("# Doc for FLOWS"), "outer fence should be stripped");

    let calls = client.calls.lock().unwrap();
    let (module_calls, synth): (Vec<_>, Vec<_>) = calls.iter().partition(|c| c.system == *prompts::MODULE_SYSTEM);
    assert_eq!(module_calls.len(), 4);
    assert!(module_calls.iter().all(|c| c.model == config.fast_model));
    assert_eq!(synth.len(), 3);
    assert!(synth.iter().all(|c| c.model == config.smart_model));
    assert!(synth[0].user.contains("=== README.md ==="), "manifests should be included");
    assert!(!synth[0].user.contains("class AuthService"), "overview calls must not resend raw code");
}

#[test]
fn analyzer_isolates_module_failures() {
    let root = shop();
    let config = quiet_config();
    let client = FakeClient { fail_on: Some("Module: app/models".into()), ..Default::default() };
    let r = Analyzer::new(root.path(), "shop", &config).quiet().run(&client).unwrap();
    assert_eq!(r.failures, ["app/models"]);
    assert_eq!(r.documents.len(), 3);
}

#[test]
fn analyzer_keeps_unparseable_reply_as_notes() {
    let root = shop();
    let config = quiet_config();
    let client = FakeClient { module_reply: Some("Sorry, here is prose instead of JSON.".into()), ..Default::default() };
    let r = Analyzer::new(root.path(), "shop", &config).quiet().run(&client).unwrap();
    assert_eq!(r.summaries[0]["parse_error"], true);
    assert!(r.summaries[0]["notes"].as_str().unwrap().contains("prose instead"));
}

#[test]
fn analyzer_refuses_over_budget_before_calling() {
    let root = shop();
    let config = Config { max_input_tokens: 10, ..quiet_config() };
    let client = FakeClient::default();
    let err = Analyzer::new(root.path(), "shop", &config).quiet().run(&client).err().unwrap();
    assert!(err.to_string().contains("exceeds the limit"));
    assert!(client.calls.lock().unwrap().is_empty());
}

#[test]
fn analyzer_refuses_too_many_files() {
    let root = shop();
    let config = Config { max_files: 2, ..quiet_config() };
    assert!(Analyzer::new(root.path(), "shop", &config).plan().is_err());
}

#[test]
fn parse_json_object_tolerates_fences_and_chatter() {
    let v = parse_json_object("Here you go:\n```json\n{\"a\": 1}\n```\nThanks").unwrap();
    assert_eq!(v, json!({"a": 1}));
}

// ---- writer ----------------------------------------------------------------

#[test]
fn writer_writes_full_doc_set() {
    let root = make_repo(&[("app/models/user.rb", b"class User; end\n"), ("lib/tax.rb", b"RATE = 0.1\n")]);
    let out = tempfile::tempdir().unwrap();
    let config = quiet_config();
    let client = FakeClient { fail_on: Some("Module: lib".into()), ..Default::default() };
    let r = Analyzer::new(root.path(), "demo", &config).quiet().run(&client).unwrap();
    let info = SourceInfo {
        name: "demo".into(),
        url: Some("https://github.com/acme/demo".into()),
        commit: Some("abcdef1234567890".into()),
    };
    writer::write(out.path(), &r, &info, &config, Some(client.usage())).unwrap();

    let mut top: Vec<_> = fs::read_dir(out.path()).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
    top.sort();
    assert_eq!(top, ["ARCHITECTURE.md", "FLOWS.md", "ONBOARDING.md", "README.md", "modules", "owlmap.json"]);
    let mut mods: Vec<_> = fs::read_dir(out.path().join("modules")).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
    mods.sort();
    assert_eq!(mods, ["app-models.md", "lib.md"]);

    let readme = fs::read_to_string(out.path().join("README.md")).unwrap();
    assert!(readme.contains("[app/models](modules/app-models.md)"));
    assert!(readme.contains("_summary failed_"));
    assert!(readme.contains("`abcdef123456`"));
    let module = fs::read_to_string(out.path().join("modules/app-models.md")).unwrap();
    assert!(module.contains("## Handle with care"));
    assert!(module.contains("`a.rb` — main"));
    let meta: serde_json::Value = serde_json::from_str(&fs::read_to_string(out.path().join("owlmap.json")).unwrap()).unwrap();
    assert_eq!(meta["failed_modules"], json!(["lib"]));
    assert!(meta["generated_at"].as_str().unwrap().ends_with('Z'));
}

// ---- Claude Code backend ---------------------------------------------------

/// A fake `claude` that records its arguments and answers like print mode.
const STUB: &str = r#"#!/usr/bin/env python3
import json, os, sys
args = sys.argv[1:]
if args == ["--version"]:
    print("9.9.9 (Claude Code)"); sys.exit(0)
mode = os.environ.get("STUB_MODE", "ok")
if mode == "old" and "--safe-mode" in args:
    print("error: unknown option '--safe-mode'", file=sys.stderr); sys.exit(1)
stdin = sys.stdin.read()
system = open(args[args.index("--system-prompt-file") + 1]).read()
json.dump({"args": args, "stdin": stdin, "system": system, "cwd": os.getcwd()}, open(os.environ["STUB_LOG"], "w"))
if mode == "error":
    print(json.dumps({"type": "result", "is_error": True, "result": "Not logged in"})); sys.exit(1)
print(json.dumps({"type": "result", "subtype": "success", "is_error": False, "result": "echo: " + stdin,
                  "usage": {"input_tokens": 10, "output_tokens": 5, "cache_read_input_tokens": 2}}))
"#;

struct Stub {
    _dir: tempfile::TempDir,
    bin: PathBuf,
    log: PathBuf,
}

fn stub(mode: &str) -> Stub {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("claude");
    fs::write(&bin, STUB).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let log = dir.path().join(format!("log-{mode}.json"));
    Stub { _dir: dir, bin, log }
}

/// Runs the client against the stub; STUB_MODE/STUB_LOG are passed per process.
fn call_stub(s: &Stub, mode: &str) -> Result<(String, Usage)> {
    // Environment is process-wide; serialise the tests that touch it.
    static ENV: Mutex<()> = Mutex::new(());
    let _g = ENV.lock().unwrap();
    std::env::set_var("STUB_MODE", mode);
    std::env::set_var("STUB_LOG", &s.log);
    let client = ClaudeCodeClient::new(Some(s.bin.display().to_string()))?;
    let reply = client.complete("claude-haiku-5-5", "SYS PROMPT", "hello code", 100)?;
    Ok((reply, client.usage()))
}

fn log_of(s: &Stub) -> serde_json::Value {
    serde_json::from_str(&fs::read_to_string(&s.log).unwrap()).unwrap()
}

#[cfg(unix)]
#[test]
fn claude_code_sends_prompt_on_stdin_without_tools() {
    let s = stub("ok");
    let (reply, usage) = call_stub(&s, "ok").unwrap();
    assert_eq!(reply, "echo: hello code");
    let log = log_of(&s);
    let args: Vec<String> = serde_json::from_value(log["args"].clone()).unwrap();
    let after = |flag: &str| args[args.iter().position(|a| a == flag).unwrap() + 1].clone();
    assert_eq!(log["system"], "SYS PROMPT");
    assert_eq!(log["stdin"], "hello code");
    assert_eq!(after("--tools"), "");
    assert_eq!(after("--model"), "claude-haiku-5-5");
    assert_eq!(after("--output-format"), "json");
    for f in ["-p", "--strict-mcp-config", "--no-session-persistence", "--safe-mode"] {
        assert!(args.iter().any(|a| a == f), "{f}");
    }
    let cwd = PathBuf::from(log["cwd"].as_str().unwrap());
    assert!(cwd.file_name().unwrap().to_string_lossy().starts_with("owlmap-cc-"), "runs in its own temp folder");
    assert!(!Path::new(&cwd).exists(), "temp folder is removed after the call");
    assert_eq!((usage.input_tokens, usage.cache_read_tokens, usage.calls), (10, 2, 1));
}

#[cfg(unix)]
#[test]
fn claude_code_falls_back_when_safe_mode_is_unknown() {
    let s = stub("old");
    let (reply, _) = call_stub(&s, "old").unwrap();
    assert_eq!(reply, "echo: hello code");
    assert!(!log_of(&s)["args"].as_array().unwrap().iter().any(|a| a == "--safe-mode"));
}

#[cfg(unix)]
#[test]
fn claude_code_reports_errors() {
    let s = stub("error");
    let err = call_stub(&s, "error").err().unwrap();
    assert!(err.to_string().contains("Not logged in"), "{err}");
}

#[test]
fn claude_code_missing_binary() {
    assert!(ClaudeCodeClient::new(Some("/definitely/not/claude".into())).is_err());
}
