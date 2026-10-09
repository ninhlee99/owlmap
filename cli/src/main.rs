use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, ValueEnum};
use owlmap::analyzer::{Analyzer, Plan};
use owlmap::cache::CacheStore;
use owlmap::client::{ApiClient, ClaudeCodeClient, Llm};
use owlmap::config::Config;
use owlmap::i18n::Lang;
use owlmap::repo_source::{self, RepoSource};
use owlmap::scanner::{Detail, ScanOptions};
use owlmap::workspace::{self, Repo};
use owlmap::writer::{self, SourceInfo};

#[derive(Clone, Copy, Debug, PartialEq, ValueEnum)]
enum Backend {
    /// api if ANTHROPIC_API_KEY is set, otherwise claude-code
    Auto,
    /// Claude API with ANTHROPIC_API_KEY (required for any hosted/shared use)
    Api,
    /// The `claude` CLI on this machine, signed in with your own account; personal runs only
    ClaudeCode,
}

const AFTER_HELP: &str = "\
Examples:
  owlmap https://github.com/sinatra/sinatra
  owlmap ../my-rails-app --out docs/owlmap --lang vi
  owlmap ../candidate ../company ../api --out docs/system     # several repos + SYSTEM.md
  owlmap .                      # in a project folder holding several repos: maps them all into ./owlmap
  owlmap ../big-monorepo --exclude 'plugins/**' --skip-tests --dry-run

Large repositories: migrations, translations, fixtures and generated code are
skipped (see --all-files); long files are read as outlines; above 30 modules,
summaries are rolled up by area first.

Re-running into the same --out folder only sends what changed. Progress is
saved as the run goes, so an interrupted run (usage limit, network) resumes
where it stopped. Use --fresh to ignore the cache.";

/// OwlMap — turn one or more codebases into a navigable map.
#[derive(Parser, Debug)]
#[command(version, after_help = AFTER_HELP)]
struct Args {
    /// Public GitHub URLs or local folders [default: .]. A folder that is not a
    /// repository but holds several is expanded into them.
    #[arg(num_args = 0.., default_value = ".")]
    targets: Vec<String>,
    /// Where to write the docs [default: <project>/owlmap for a project folder,
    /// owlmap-docs/<repo> for one repository, owlmap-docs/workspace for several]
    #[arg(short, long)]
    out: Option<PathBuf>,
    /// Scan and estimate cost without calling Claude
    #[arg(long)]
    dry_run: bool,
    /// How to reach Claude
    #[arg(long, value_enum, default_value_t = Backend::Auto)]
    backend: Backend,
    /// Language of the generated docs (code names are never translated)
    #[arg(long, value_enum, default_value_t = Lang::En)]
    lang: Lang,
    /// Ignore the cache from previous runs and re-analyse everything
    #[arg(long)]
    fresh: bool,
    /// Only read paths matching this glob (repeatable), e.g. 'app/**'
    #[arg(long, value_name = "GLOB")]
    include: Vec<String>,
    /// Skip paths matching this glob (repeatable), e.g. 'plugins/**'
    #[arg(long, value_name = "GLOB")]
    exclude: Vec<String>,
    /// How much of each file to read: quick (outlines), standard, deep (full files)
    #[arg(long, value_enum, default_value_t = Detail::Standard)]
    detail: Detail,
    /// Leave test files out entirely (they are otherwise read from their first lines)
    #[arg(long)]
    skip_tests: bool,
    /// Also read migrations, translations, fixtures and generated code
    #[arg(long)]
    all_files: bool,
    /// Refuse a repository with more source files than this, after filtering
    #[arg(long, default_value_t = 20_000)]
    max_files: usize,
    /// Stop before starting if the estimated input for the whole run is higher
    #[arg(long, default_value_t = 20_000_000)]
    max_input_tokens: u64,
    /// Parallel Claude calls [default: 4, or 2 with claude-code]
    #[arg(long, value_parser = clap::value_parser!(u16).range(1..=16))]
    concurrency: Option<u16>,
    /// Model for module and area summaries [default: claude-haiku-5-5, or $OWLMAP_FAST_MODEL]
    #[arg(long)]
    fast_model: Option<String>,
    /// Model for the overview documents [default: claude-sonnet-5-5, or $OWLMAP_SMART_MODEL]
    #[arg(long)]
    smart_model: Option<String>,
    /// In a project folder, also map git worktrees (skipped by default: they are
    /// extra checkouts of another repository)
    #[arg(long)]
    include_worktrees: bool,
    /// List every module in the dry run, even for several repositories
    #[arg(short, long)]
    verbose: bool,
}

fn main() -> ExitCode {
    let args = Args::parse();
    match run(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("owlmap: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Args) -> anyhow::Result<ExitCode> {
    let mut config = Config {
        max_files: args.max_files,
        max_input_tokens: args.max_input_tokens,
        lang: args.lang,
        scan: ScanOptions {
            detail: args.detail,
            skip_tests: args.skip_tests,
            all_files: args.all_files,
            include: args.include.clone(),
            exclude: args.exclude.clone(),
            ..ScanOptions::default()
        },
        ..Config::default()
    };
    if let Some(m) = &args.fast_model {
        config.fast_model = m.clone();
    }
    if let Some(m) = &args.smart_model {
        config.smart_model = m.clone();
    }

    // A project folder holding several repositories expands into them.
    let mut targets: Vec<String> = Vec::new();
    let mut parent: Option<PathBuf> = None;
    for t in &args.targets {
        match repo_source::discover(std::path::Path::new(t)) {
            Some(found) => {
                let names = |v: &[PathBuf]| -> String {
                    v.iter().filter_map(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).collect::<Vec<_>>().join(", ")
                };
                let mut chosen = found.repos.clone();
                if !found.worktrees.is_empty() {
                    if args.include_worktrees {
                        chosen.extend(found.worktrees.iter().cloned());
                    } else {
                        eprintln!(
                            "Skipping {} git worktree(s) in {t} (extra checkouts of another repository): {} — add --include-worktrees to map them too",
                            found.worktrees.len(),
                            names(&found.worktrees)
                        );
                    }
                }
                if chosen.is_empty() {
                    anyhow::bail!("{t} only holds git worktrees; add --include-worktrees, or name the folders to map");
                }
                eprintln!("Found {} repositories in {t}: {}", chosen.len(), names(&chosen));
                if args.targets.len() == 1 {
                    parent = Some(std::path::Path::new(t).canonicalize()?);
                }
                targets.extend(chosen.iter().map(|p| p.display().to_string()));
            }
            None => targets.push(t.clone()),
        }
    }

    // Resolve every target first, so a typo fails before any work starts.
    let mut sources: Vec<RepoSource> = Vec::new();
    for t in &targets {
        let mut s = RepoSource::resolve(t)?;
        let base = s.name.clone();
        let mut n = 2;
        while sources.iter().any(|o| o.name == s.name) {
            s.name = format!("{base}-{n}");
            n += 1;
        }
        let at = s.commit.as_deref().map(|c| format!(" @ {}", &c[..c.len().min(12)])).unwrap_or_default();
        eprintln!("Reading {} as \"{}\"{at}", s.url.clone().unwrap_or_else(|| s.path.display().to_string()), s.name);
        sources.push(s);
    }
    let multi = sources.len() > 1;
    // Default output: <project>/owlmap for a project folder, otherwise owlmap-docs/…
    let out = args.out.clone().unwrap_or_else(|| match &parent {
        Some(p) => p.join("owlmap"),
        None => PathBuf::from("owlmap-docs").join(if multi { "workspace".to_string() } else { sources[0].name.clone() }),
    });
    let repo_out = |name: &str| if multi { out.join(name) } else { out.clone() };

    let store = CacheStore::open(&out, args.fresh);
    let backend = match args.backend {
        Backend::Auto if std::env::var("ANTHROPIC_API_KEY").map(|k| k.is_empty()).unwrap_or(true) => Backend::ClaudeCode,
        Backend::Auto => Backend::Api,
        b => b,
    };
    // Gentler on subscription limits with Claude Code.
    config.concurrency = args.concurrency.map(usize::from).unwrap_or(if backend == Backend::ClaudeCode { 2 } else { 4 });
    let config = config;

    // Plan everything and check one budget for the whole run.
    let analyzers: Vec<Analyzer> = sources.iter().map(|s| Analyzer::new(&s.path, &s.name, &config).with_store(&store)).collect();
    let mut plans: Vec<Plan> = Vec::new();
    let mut total = 0;
    let mut total_uncached = 0;
    for (a, s) in analyzers.iter().zip(&sources) {
        let plan = a.plan()?;
        let pending = a.pending(&plan);
        let skipped = if plan.skipped.is_empty() {
            "none".to_string()
        } else {
            plan.skipped.iter().map(|(k, v)| format!("{v} {k}")).collect::<Vec<_>>().join(", ")
        };
        eprintln!("[{}] {} source files in {} modules (skipped: {skipped})", s.name, plan.files.len(), plan.modules.len());
        let cached = if pending.cached_modules > 0 {
            format!(" ({} of {} modules unchanged; ~{} without cache)", pending.cached_modules, plan.modules.len(), plan.estimated_input_tokens)
        } else {
            String::new()
        };
        eprintln!("[{}] Estimated input: ~{} tokens{cached}", s.name, pending.estimated_input_tokens);
        total += pending.estimated_input_tokens;
        total_uncached += plan.estimated_input_tokens;
        plans.push(plan);
    }
    if multi {
        eprintln!(
            "Total estimated input: ~{total} tokens (~{total_uncached} without cache), plus ~{} for SYSTEM.md if anything changed",
            workspace::system_estimate(sources.len())
        );
    }

    if args.dry_run {
        if !multi || args.verbose {
            for (plan, s) in plans.iter().zip(&sources) {
                for m in &plan.modules {
                    eprintln!("  [{}] {:<44} {:>5} files  ~{} tokens", s.name, m.name, m.files.len(), owlmap::estimate_tokens(m.bytes()));
                }
            }
        }
        return Ok(ExitCode::SUCCESS);
    }
    if total > config.max_input_tokens {
        anyhow::bail!(
            "estimated {total} input tokens exceeds the limit of {}. Narrow it with --include/--exclude/--skip-tests, \
             or raise --max-input-tokens.",
            config.max_input_tokens
        );
    }

    let client: Box<dyn Llm> = if backend == Backend::ClaudeCode {
        eprintln!("Using Claude Code on this machine (your own account; for personal runs only)");
        Box::new(ClaudeCodeClient::new(None)?)
    } else {
        eprintln!("Using the Claude API (ANTHROPIC_API_KEY)");
        Box::new(ApiClient::from_env()?)
    };

    let mut results = Vec::new();
    for ((a, plan), s) in analyzers.iter().zip(plans).zip(&sources) {
        let result = a.run_checked(plan, client.as_ref());
        if let Some(reason) = &result.stopped {
            store.save_progress()?;
            report_usage(client.as_ref());
            eprintln!(
                "\nStopped: {reason}\nEverything finished so far is saved in {}. Run the same command again later to continue \
                 from here.",
                out.join(owlmap::cache::FILE_NAME).display()
            );
            return Ok(ExitCode::from(2));
        }
        let dir = repo_out(&s.name);
        let info = SourceInfo { name: s.name.clone(), url: s.url.clone(), commit: s.commit.clone() };
        writer::write(&dir, &result, &info, &config, Some(client.usage()))?;
        eprintln!(
            "[{}] Done: {} (reused {} modules, {} documents from cache)",
            s.name,
            dir.display(),
            result.reused_modules,
            result.reused_documents
        );
        if !result.failures.is_empty() {
            eprintln!("[{}] {} module(s) failed and will be retried next run: {}", s.name, result.failures.len(), result.failures.join(", "));
        }
        store.save_progress()?;
        results.push(result);
    }

    if multi {
        let repos: Vec<Repo> = sources
            .iter()
            .zip(&results)
            .map(|(s, r)| Repo { name: s.name.clone(), root: s.path.clone(), url: s.url.clone(), commit: s.commit.clone(), result: r })
            .collect();
        eprintln!("Writing SYSTEM.md with {}…", config.smart_model);
        match workspace::system_doc_with(&repos, parent.as_deref(), &config, &store, client.as_ref()) {
            Ok((text, reused)) => {
                std::fs::write(out.join("SYSTEM.md"), text)?;
                if reused {
                    eprintln!("SYSTEM.md unchanged (from cache)");
                }
            }
            Err(e) => eprintln!("Could not write SYSTEM.md: {e:#}"),
        }
        std::fs::write(out.join("README.md"), workspace::index_markdown(&repos, &config))?;
    }

    store.finish()?;
    eprintln!("Done: {}", std::fs::canonicalize(&out).unwrap_or(out.clone()).display());
    report_usage(client.as_ref());
    Ok(ExitCode::SUCCESS)
}

fn report_usage(client: &dyn Llm) {
    let u = client.usage();
    eprintln!("Usage: {} calls, {} input + {} cached + {} output tokens", u.calls, u.input_tokens, u.cache_read_tokens, u.output_tokens);
}
