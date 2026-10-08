use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, ValueEnum};
use owlmap::analyzer::Analyzer;
use owlmap::client::{ApiClient, ClaudeCodeClient, Llm};
use owlmap::config::Config;
use owlmap::repo_source::RepoSource;
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

/// OwlMap — turn a codebase into a navigable map.
#[derive(Parser, Debug)]
#[command(
    version,
    after_help = "Examples:\n  owlmap https://github.com/sinatra/sinatra\n  owlmap ../my-rails-app --out docs/owlmap\n  \
owlmap https://github.com/rack/rack --dry-run\n  owlmap ../my-app --backend claude-code"
)]
struct Args {
    /// Public GitHub URL (https://github.com/owner/repo) or a local folder
    target: String,
    /// Where to write the docs [default: owlmap-docs/<repo>]
    #[arg(short, long)]
    out: Option<PathBuf>,
    /// Scan and estimate cost without calling Claude
    #[arg(long)]
    dry_run: bool,
    /// How to reach Claude
    #[arg(long, value_enum, default_value_t = Backend::Auto)]
    backend: Backend,
    /// Refuse repos with more source files than this
    #[arg(long, default_value_t = 500)]
    max_files: usize,
    /// Abort if the input estimate is higher than this
    #[arg(long, default_value_t = 600_000)]
    max_input_tokens: u64,
    /// Parallel module summaries [default: 4, or 2 with claude-code]
    #[arg(long, value_parser = clap::value_parser!(u16).range(1..=16))]
    concurrency: Option<u16>,
    /// Model for module summaries [default: claude-haiku-5-5, or $OWLMAP_FAST_MODEL]
    #[arg(long)]
    fast_model: Option<String>,
    /// Model for the overview documents [default: claude-sonnet-5-5, or $OWLMAP_SMART_MODEL]
    #[arg(long)]
    smart_model: Option<String>,
}

fn main() -> ExitCode {
    let args = Args::parse();
    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("owlmap: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Args) -> anyhow::Result<()> {
    let mut config = Config { max_files: args.max_files, max_input_tokens: args.max_input_tokens, ..Config::default() };
    if let Some(m) = args.fast_model {
        config.fast_model = m;
    }
    if let Some(m) = args.smart_model {
        config.smart_model = m;
    }

    let source = RepoSource::resolve(&args.target)?;
    let shown = source.url.clone().unwrap_or_else(|| source.path.display().to_string());
    let at = source.commit.as_deref().map(|c| format!(" @ {}", &c[..c.len().min(12)])).unwrap_or_default();
    eprintln!("Reading {shown}{at}");

    let client: Option<Box<dyn Llm>> = if args.dry_run {
        None
    } else {
        let backend = match args.backend {
            Backend::Auto if std::env::var("ANTHROPIC_API_KEY").map(|k| k.is_empty()).unwrap_or(true) => Backend::ClaudeCode,
            Backend::Auto => Backend::Api,
            b => b,
        };
        if backend == Backend::ClaudeCode {
            config.concurrency = args.concurrency.map(usize::from).unwrap_or(2); // gentle on subscription limits
            eprintln!("Using Claude Code on this machine (your own account; for personal runs only)");
            Some(Box::new(ClaudeCodeClient::new(None)?))
        } else {
            config.concurrency = args.concurrency.map(usize::from).unwrap_or(4);
            eprintln!("Using the Claude API (ANTHROPIC_API_KEY)");
            Some(Box::new(ApiClient::from_env()?))
        }
    };

    let analyzer = Analyzer::new(&source.path, &source.name, &config);
    let plan = analyzer.plan()?;
    let skipped = if plan.skipped.is_empty() {
        "none".to_string()
    } else {
        plan.skipped.iter().map(|(k, v)| format!("{v} {k}")).collect::<Vec<_>>().join(", ")
    };
    eprintln!("{} source files in {} modules (skipped: {skipped})", plan.files.len(), plan.modules.len());
    eprintln!("Estimated input: ~{} tokens", plan.estimated_input_tokens);

    let Some(client) = client else {
        for m in &plan.modules {
            eprintln!("  {:<40} {:>4} files  ~{} tokens", m.name, m.files.len(), owlmap::estimate_tokens(m.bytes()));
        }
        return Ok(());
    };

    let result = analyzer.run(client.as_ref())?;
    let out = args.out.unwrap_or_else(|| PathBuf::from("owlmap-docs").join(&source.name));
    let info = SourceInfo { name: source.name.clone(), url: source.url.clone(), commit: source.commit.clone() };
    let usage = client.usage();
    writer::write(&out, &result, &info, &config, Some(usage))?;

    eprintln!("Done: {}", std::fs::canonicalize(&out).unwrap_or(out).display());
    eprintln!(
        "Usage: {} calls, {} input + {} cached + {} output tokens",
        usage.calls, usage.input_tokens, usage.cache_read_tokens, usage.output_tokens
    );
    if !result.failures.is_empty() {
        eprintln!("{} module(s) failed: {}", result.failures.len(), result.failures.join(", "));
    }
    Ok(())
}
