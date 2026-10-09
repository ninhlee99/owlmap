//! Offline tests: a fake Claude stands in for the API, and a stub `claude`
//! executable stands in for Claude Code, so nothing here costs tokens.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{bail, Result};
use owlmap::analyzer::{parse_json_object, Analyzer};
use owlmap::cache::CacheStore;
use owlmap::i18n::Lang;
use owlmap::client::{ClaudeCodeClient, Llm, Usage};
use owlmap::config::Config;
use owlmap::grouper::{slug, Grouper};
use owlmap::prompts;
use owlmap::repo_source::{RepoSource, GITHUB_URL};
use owlmap::scanner::{Detail, ReadMode, ScanOptions, Scanner, SourceFile};
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
    /// Every call after this many fails with a usage-limit error.
    limit_after: Option<usize>,
    calls: Mutex<Vec<Call>>,
}

impl Llm for FakeClient {
    fn complete(&self, model: &str, system: &str, user: &str, _max: u32) -> Result<String> {
        let n = {
            let mut calls = self.calls.lock().unwrap();
            calls.push(Call { model: model.into(), system: system.into(), user: user.into() });
            calls.len()
        };
        if self.limit_after.is_some_and(|l| n > l) {
            bail!("Claude Code call failed: Claude AI usage limit reached");
        }
        if system == prompts::AREA_SYSTEM.as_str() {
            return Ok("{\"purpose\": \"An area.\", \"components\": [], \"interfaces\": [\"I\"], \"depends_on\": [], \"data\": [], \"risks\": []}".into());
        }
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
    SourceFile { path: path.into(), size, language: "Ruby".into(), ..Default::default() }
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
    let f = SourceFile { path: "spec/big_spec.rb".into(), size: body.len() as u64, language: "Ruby".into(), test: true, mode: ReadMode::Head(60) };
    let text = f.read(root.path());
    assert!(text.contains("line 59\n") && !text.contains("line 60\n"));
    assert!(text.contains("40 more lines not shown"));
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
        SourceFile { test: true, mode: ReadMode::TestNames(12), ..sf("contrib/spec/b_spec.rb", 100) },
        sf("lib/a.rb", 5_000),
        SourceFile { test: true, mode: ReadMode::TestNames(12), ..sf("test/a_test.rb", 90_000) },
    ];
    let mods = Grouper::new(50_000, 3_000).group(&files);
    assert_eq!(names(&mods), ["contrib (tests)", "lib", "project (tests)"]);
    assert_eq!(mods[2].bytes(), 480, "test files count at their truncated size");
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
    assert_eq!(synth[0].user.matches("<module_summaries>").count(), 1, "the code block is tagged exactly once");
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

// ---- incremental runs ------------------------------------------------------

fn counts(client: &FakeClient) -> (usize, usize) {
    let calls = client.calls.lock().unwrap();
    let modules = calls.iter().filter(|c| c.system == *prompts::MODULE_SYSTEM).count();
    (modules, calls.len() - modules)
}

/// Runs into `out`, carrying the cache between runs exactly as the CLI does.
fn run_into(root: &Path, out: &Path, config: &Config, client: &FakeClient) -> owlmap::analyzer::RunResult {
    let store = CacheStore::open(out, false);
    let r = Analyzer::new(root, "shop", config).quiet().with_store(&store).run(client).unwrap();
    if r.stopped.is_some() {
        store.save_progress().unwrap();
        return r;
    }
    let info = SourceInfo { name: "shop".into(), url: None, commit: None };
    writer::write(out, &r, &info, config, None).unwrap();
    store.finish().unwrap();
    r
}

fn pending_of(root: &Path, out: &Path, config: &Config) -> owlmap::analyzer::Pending {
    let store = CacheStore::open(out, false);
    let a = Analyzer::new(root, "shop", config).with_store(&store);
    let plan = a.plan().unwrap();
    a.pending(&plan)
}

#[test]
fn second_run_reuses_everything_when_nothing_changed() {
    let root = shop();
    let out = tempfile::tempdir().unwrap();
    let config = quiet_config();

    let first = FakeClient::default();
    let r1 = run_into(root.path(), out.path(), &config, &first);
    assert_eq!(counts(&first), (4, 3));
    assert_eq!((r1.reused_modules, r1.reused_documents), (0, 0));

    let second = FakeClient::default();
    let r2 = run_into(root.path(), out.path(), &config, &second);
    assert_eq!(counts(&second), (0, 0), "nothing changed, so nothing is sent");
    assert_eq!((r2.reused_modules, r2.reused_documents), (4, 3));
    assert_eq!(r1.documents, r2.documents);
    assert_eq!(pending_of(root.path(), out.path(), &config).estimated_input_tokens, 0, "an unchanged repo costs nothing");
    assert_eq!(r2.summaries[0]["purpose"], "Handles things. More detail.");
}

#[test]
fn changing_one_file_resends_only_its_module_and_the_overviews() {
    let root = shop();
    let out = tempfile::tempdir().unwrap();
    let config = quiet_config();
    run_into(root.path(), out.path(), &config, &FakeClient::default());

    fs::write(root.path().join("app/models/user.rb"), "class User; has_many :orders; end\n").unwrap();
    let client = FakeClient::default();
    let r = run_into(root.path(), out.path(), &config, &client);
    assert!(client.calls.lock().unwrap()[0].user.contains("Module: app/models"));
    assert_eq!(r.reused_modules, 3);
    // The fake returns the same summary, so the overview inputs are unchanged and reused.
    assert_eq!(counts(&client), (1, 0));
    assert_eq!(r.reused_documents, 3);

    // A new file changes the file tree the overviews are written from.
    fs::write(root.path().join("app/models/order.rb"), "class Order; end\n").unwrap();
    let client = FakeClient::default();
    run_into(root.path(), out.path(), &config, &client);
    assert_eq!(counts(&client), (1, 3));

    assert_eq!(pending_of(root.path(), out.path(), &config).cached_modules, 4, "the cache now reflects the new content");
}

#[test]
fn changing_language_or_model_invalidates_the_cache() {
    let root = shop();
    let out = tempfile::tempdir().unwrap();
    run_into(root.path(), out.path(), &quiet_config(), &FakeClient::default());

    let vi = FakeClient::default();
    run_into(root.path(), out.path(), &Config { lang: Lang::Vi, ..quiet_config() }, &vi);
    assert_eq!(counts(&vi), (4, 3));

    let other_model = FakeClient::default();
    run_into(root.path(), out.path(), &Config { lang: Lang::Vi, fast_model: "claude-sonnet-5-5".into(), ..quiet_config() }, &other_model);
    assert_eq!(counts(&other_model).0, 4, "module summaries depend on the fast model");
}

#[test]
fn failed_modules_are_retried_next_time() {
    let root = shop();
    let out = tempfile::tempdir().unwrap();
    let config = quiet_config();
    let failing = FakeClient { fail_on: Some("Module: app/models".into()), ..Default::default() };
    run_into(root.path(), out.path(), &config, &failing);

    let client = FakeClient::default();
    let r = run_into(root.path(), out.path(), &config, &client);
    assert_eq!(counts(&client).0, 1, "only the module that failed is sent again");
    assert!(r.failures.is_empty());
}

#[test]
fn stale_module_notes_are_removed() {
    let root = shop();
    let out = tempfile::tempdir().unwrap();
    let config = quiet_config();
    run_into(root.path(), out.path(), &config, &FakeClient::default());
    assert!(out.path().join("modules/app-services.md").exists());

    fs::remove_dir_all(root.path().join("app/services")).unwrap();
    run_into(root.path(), out.path(), &config, &FakeClient::default());
    assert!(!out.path().join("modules/app-services.md").exists());
}

// ---- languages -------------------------------------------------------------

#[test]
fn language_reaches_prompts_and_labels() {
    let root = shop();
    let out = tempfile::tempdir().unwrap();
    let config = Config { lang: Lang::Vi, ..quiet_config() };
    let client = FakeClient::default();
    run_into(root.path(), out.path(), &config, &client);

    assert!(client.calls.lock().unwrap().iter().all(|c| c.user.contains("Write all prose in Vietnamese")));
    let readme = fs::read_to_string(out.path().join("README.md")).unwrap();
    assert!(readme.contains("Tài liệu được tạo cho") && readme.contains("| Module | Số tệp | Mục đích |"), "{readme}");
    let note = fs::read_to_string(out.path().join("modules/app-models.md")).unwrap();
    assert!(note.contains("## Cần cẩn thận") && note.contains("## Tệp chính"));
    let meta: serde_json::Value = serde_json::from_str(&fs::read_to_string(out.path().join("owlmap.json")).unwrap()).unwrap();
    assert_eq!(meta["lang"], "vi");

    let ja = Config { lang: Lang::Ja, ..quiet_config() };
    run_into(root.path(), out.path(), &ja, &FakeClient::default());
    assert!(fs::read_to_string(out.path().join("modules/app-models.md")).unwrap().contains("## 主要ファイル"));
}

#[test]
fn cache_keys_are_length_prefixed() {
    assert_ne!(owlmap::cache::key(&["ab", "c"]), owlmap::cache::key(&["a", "bc"]));
    assert_eq!(owlmap::cache::key(&["x"]).len(), 64);
}

// ---- large repositories ----------------------------------------------------

#[test]
fn low_signal_bulk_and_filters_are_skipped() {
    let root = make_repo(&[
        ("app/models/user.rb", b"class User; end\n"),
        ("db/migrate/001_create_users.rb", b"class CreateUsers; end\n"),
        ("config/locales/ja.yml", b"ja:\n  hello: x\n"),
        ("config/locales/en.yml", b"en:\n  hello: x\n"),
        ("spec/fixtures/users.yml", b"one: {}\n"),
        ("public/index.html", b"<html></html>\n"),
        ("plugins/chat/plugin.rb", b"module Chat; end\n"),
        ("spec/models/user_spec.rb", b"describe User do\n  it 'works' do\n  end\nend\n"),
    ]);
    let scan = |o: ScanOptions| Scanner::with_options(root.path(), o).unwrap().scan();
    let paths = |r: owlmap::scanner::ScanResult| r.files.into_iter().map(|f| f.path).collect::<Vec<_>>();

    let r = scan(ScanOptions::default());
    assert_eq!(r.skipped["low_signal"], 5);
    assert_eq!(paths(r), ["app/models/user.rb", "plugins/chat/plugin.rb", "spec/models/user_spec.rb"]);

    let r = scan(ScanOptions { all_files: true, ..Default::default() });
    assert_eq!(r.files.len(), 8);

    let r = scan(ScanOptions { exclude: vec!["plugins".into()], skip_tests: true, ..Default::default() });
    assert_eq!(paths(r), ["app/models/user.rb"]);

    let r = scan(ScanOptions { include: vec!["app/**".into(), "spec".into()], ..Default::default() });
    assert_eq!(paths(r), ["app/models/user.rb", "spec/models/user_spec.rb"]);

    assert!(Scanner::with_options(root.path(), ScanOptions { include: vec!["[".into()], ..Default::default() }).is_err());
}

#[test]
fn long_files_are_read_as_outlines_and_tests_as_names() {
    let mut code = String::from("# frozen_string_literal: true\nclass Billing\n");
    for i in 0..200 {
        code += &format!("  def charge_{i}(amount)\n    total = amount * {i}\n    tax = total * 0.1\n    fee = 30\n    log(total)\n    notify(total)\n    total + tax + fee\n  end\n\n");
    }
    code += "end\n";
    let mut spec = String::from("require 'rails_helper'\nRSpec.describe Billing do\n");
    for i in 0..30 {
        spec += &format!("  it 'charges case {i}' do\n    expect(1).to eq(1)\n  end\n");
    }
    spec += "end\n";
    let root = make_repo(&[("app/billing.rb", code.as_bytes()), ("spec/billing_spec.rb", spec.as_bytes())]);

    let files = Scanner::with_options(root.path(), ScanOptions::default()).unwrap().scan().files;
    let billing = files.iter().find(|f| f.path == "app/billing.rb").unwrap();
    assert!(matches!(billing.mode, ReadMode::Outline { .. }));
    let text = billing.read(root.path());
    assert!(text.contains("declarations below with line numbers"));
    assert!(text.contains("def charge_199(amount)"), "every declaration is listed");
    assert!(!text.contains("amount * 199"), "bodies are not");
    assert!(text.len() < code.len() / 3, "{} vs {}", text.len(), code.len());

    let spec_file = files.iter().find(|f| f.path == "spec/billing_spec.rb").unwrap();
    let names = spec_file.read(root.path());
    assert!(names.starts_with("RSpec.describe Billing do"), "{names}");
    assert!(names.contains("it 'charges case 10'") && !names.contains("expect(1)"));
    assert!(names.lines().count() <= 13);

    let deep = Scanner::with_options(root.path(), ScanOptions { detail: Detail::Deep, ..Default::default() }).unwrap().scan().files;
    assert!(matches!(deep.iter().find(|f| f.path == "spec/billing_spec.rb").unwrap().mode, ReadMode::Head(60)));
}

fn module(name: &str) -> owlmap::grouper::Module {
    owlmap::grouper::Module { name: name.into(), files: vec![sf(&format!("{name}/x.rb"), 10)] }
}

#[test]
fn areas_split_big_folders_and_merge_tiny_ones() {
    let mut mods: Vec<_> = (0..6).map(|i| module(&format!("app/models/m{i}"))).collect();
    mods.extend((0..5).map(|i| module(&format!("app/controllers/c{i}"))));
    mods.push(module("app/mailers"));
    mods.push(module("app/jobs"));
    mods.push(module("lib"));
    mods.push(module(".github"));
    mods.push(owlmap::grouper::Module { name: "project (tests)".into(), files: vec![] });
    mods.push(owlmap::grouper::Module { name: "(root)".into(), files: vec![] });

    let groups = owlmap::analyzer::area_groups(&mods, 6);
    let names: Vec<&str> = groups.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["(other)", "(root)", "(tests)", "app/controllers", "app/models"]);
    let other: Vec<&str> = groups[0].1.iter().map(|&i| mods[i].name.as_str()).collect();
    assert_eq!(other, ["app/mailers", "app/jobs", "lib", ".github"], "tiny folders at any level are gathered");
    let total: usize = groups.iter().map(|(_, m)| m.len()).sum();
    assert_eq!(total, mods.len(), "every module lands in exactly one area");
    assert!(groups.iter().all(|(_, m)| m.len() <= 6));
}

fn wide_repo() -> tempfile::TempDir {
    let files: Vec<(String, Vec<u8>)> =
        (0..12).map(|i| (format!("lib/part{i}/thing.rb"), format!("class Thing{i}; end\n").into_bytes())).collect();
    let refs: Vec<(&str, &[u8])> = files.iter().map(|(p, b)| (p.as_str(), b.as_slice())).collect();
    make_repo(&refs)
}

#[test]
fn large_repos_roll_up_into_areas_before_the_overview() {
    let root = wide_repo();
    let config = Config { rollup_threshold: 5, area_max_modules: 4, ..quiet_config() };
    let client = FakeClient::default();
    let r = Analyzer::new(root.path(), "wide", &config).quiet().run(&client).unwrap();

    assert_eq!(r.summaries.len(), 12);
    assert_eq!(r.areas.len(), 3, "12 modules in areas of at most 4");
    let calls = client.calls.lock().unwrap();
    assert_eq!(calls.iter().filter(|c| c.system == *prompts::AREA_SYSTEM).count(), 3);
    let doc = calls.iter().find(|c| c.system == *prompts::SYNTHESIS_SYSTEM).unwrap();
    assert!(doc.user.contains("<area_summaries>") && doc.user.contains("<module_index>"));
    assert_eq!(doc.user.matches("<area_summaries>").count(), 1);
    assert!(!doc.user.contains("<module_summaries>"), "the overview reads areas, not every module");
}

#[test]
fn usage_limit_stops_the_run_and_the_next_run_resumes() {
    let root = wide_repo();
    let out = tempfile::tempdir().unwrap();
    let config = Config { concurrency: 1, ..quiet_config() };

    let limited = FakeClient { limit_after: Some(5), ..Default::default() };
    let r = run_into(root.path(), out.path(), &config, &limited);
    assert!(r.stopped.as_deref().unwrap().contains("usage limit"));
    assert!(r.documents.is_empty(), "no overview from a partial run");
    assert_eq!(limited.calls.lock().unwrap().len(), 6, "one failed call, then nothing more is attempted");
    assert!(!out.path().join("ARCHITECTURE.md").exists());

    let client = FakeClient::default();
    let r = run_into(root.path(), out.path(), &config, &client);
    assert!(r.stopped.is_none());
    assert_eq!(r.reused_modules, 5, "the five finished modules come from the saved progress");
    assert_eq!(counts(&client), (7, 3));
    assert!(out.path().join("ARCHITECTURE.md").exists());
}

#[test]
fn cache_store_saves_progress_and_prunes_on_finish() {
    let dir = tempfile::tempdir().unwrap();
    let store = CacheStore::open(dir.path(), false);
    store.put("old".into(), json!(1));
    store.finish().unwrap();

    let store = CacheStore::open(dir.path(), false);
    assert_eq!(store.len_previous(), 1);
    store.put("new".into(), json!(2));
    store.save_progress().unwrap();
    assert_eq!(CacheStore::open(dir.path(), false).len_previous(), 2, "progress keeps earlier entries");
    store.finish().unwrap();
    assert_eq!(CacheStore::open(dir.path(), false).len_previous(), 1, "finish keeps only what was used");
    assert_eq!(CacheStore::open(dir.path(), true).len_previous(), 0, "--fresh starts empty");
}

// ---- several repositories --------------------------------------------------

#[test]
fn integration_signals_find_env_hosts_and_repo_mentions() {
    let root = make_repo(&[
        ("app/clients/api_client.rb", b"BASE = ENV.fetch('COMPANY_API_URL')\nHTTP.get(\"https://api.example-corp.jp/v1/jobs\")\n"),
        ("src/config.ts", b"export const url = process.env.COMPANY_API_URL;\nconst docs = 'https://github.com/x/y';\n"),
        ("config/app.yml", b"redis: ${REDIS_URL:-redis://localhost:6379}\nsso: http://localhost:3001\n"),
        ("app/models/job.rb", b"# every Company has jobs; a company page lists them\nclass Job; belongs_to :company; end\n"),
        ("app/services/sync.rb", b"SSO = ENV['COMPANY_URL']\nADMIN = 'http://company:3000/admin'\n"),
        ("spec/client_spec.rb", b"ENV['TEST_ONLY_VAR']\n"),
    ]);
    let files = Scanner::new(root.path(), 100_000).scan().files;
    let s = owlmap::workspace::signals(root.path(), &files, &["company".into(), "api".into()]);
    assert_eq!(s.env["COMPANY_API_URL"].len(), 2);
    assert!(s.env.contains_key("REDIS_URL"));
    assert!(!s.env.contains_key("TEST_ONLY_VAR"), "tests are not evidence of integrations");
    assert!(s.hosts.contains_key("api.example-corp.jp"));
    assert!(s.hosts.contains_key("localhost:3001"));
    assert!(!s.hosts.contains_key("github.com"));
    let company: Vec<&str> = s.mentions["company"].iter().map(String::as_str).collect();
    assert_eq!(company, ["app/services/sync.rb"], "model names are not service references; COMPANY_URL and //company: are");
    assert!(!s.mentions.contains_key("api"), "the word api alone is not a reference to the api repository");
}

#[test]
fn workspace_writes_system_overview_from_every_repo() {
    let candidate = make_repo(&[("app/clients/jobs.rb", b"URL = ENV['JOBS_API_URL']\nBACKUP = 'http://api:3000'\n")]);
    let api = make_repo(&[("app/controllers/jobs_controller.rb", b"class JobsController; end\n")]);
    let out = tempfile::tempdir().unwrap();
    let config = quiet_config();
    let store = CacheStore::open(out.path(), false);
    let client = FakeClient::default();

    let rc = Analyzer::new(candidate.path(), "candidate", &config).quiet().with_store(&store).run(&client).unwrap();
    let ra = Analyzer::new(api.path(), "api", &config).quiet().with_store(&store).run(&client).unwrap();
    let repos = vec![
        owlmap::workspace::Repo { name: "candidate".into(), root: candidate.path().into(), url: None, commit: None, result: &rc },
        owlmap::workspace::Repo { name: "api".into(), root: api.path().into(), url: None, commit: None, result: &ra },
    ];
    let input = owlmap::workspace::system_input(&repos);
    assert!(input.contains("<repository name=\"candidate\" docs=\"candidate/README.md\">"));
    assert!(input.contains("JOBS_API_URL"));
    assert!(input.contains("References to the other repositories as services:\n- api"), "{input}");

    let (_, reused) = owlmap::workspace::system_doc(&repos, &config, &store, &client).unwrap();
    assert!(!reused);
    let (_, reused) = owlmap::workspace::system_doc(&repos, &config, &store, &client).unwrap();
    assert!(reused, "unchanged inputs come from the cache");

    let index = owlmap::workspace::index_markdown(&repos, &config);
    assert!(index.contains("[System overview](SYSTEM.md)") && index.contains("| [api](api/README.md) |"));
}

// ---- project folders -------------------------------------------------------

#[test]
fn project_folder_expands_into_its_repositories() {
    let project = tempfile::tempdir().unwrap();
    for r in ["company", "api", "candidate"] {
        fs::create_dir_all(project.path().join(r).join(".git")).unwrap();
    }
    fs::create_dir_all(project.path().join("docs")).unwrap(); // not a repository
    fs::create_dir_all(project.path().join("owlmap")).unwrap(); // our own output
    fs::create_dir_all(project.path().join(".hidden/.git")).unwrap();
    fs::write(project.path().join("docker-compose.yml"), "services:\n  api:\n    build: ./api\n").unwrap();

    let found = owlmap::repo_source::discover(project.path()).unwrap();
    let names: Vec<String> = found.iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
    assert_eq!(names, ["api", "candidate", "company"]);

    assert!(owlmap::repo_source::discover(&project.path().join("api")).is_none(), "a repository is not expanded");
    assert!(owlmap::repo_source::discover(&project.path().join("docs")).is_none(), "nor is a folder without repositories");

    let shared = owlmap::workspace::workspace_files(project.path());
    assert!(shared.contains("=== docker-compose.yml ===") && shared.contains("build: ./api"));
}
