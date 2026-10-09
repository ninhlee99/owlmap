# OwlMap CLI

The analysis core of OwlMap, written in Rust: point it at a repository and it
writes an architecture overview, key flows with Mermaid diagrams, per-module
notes and an onboarding guide, using Claude.

Ships as a single ~4 MB binary with no runtime to install.

## Install

**Prebuilt binary:** download `owlmap-<platform>` from the
[Releases](https://github.com/ninhlee99/owlmap/releases) page (built by
`.github/workflows/cli.yml` for every `v*` tag), make it executable and put it on
your `PATH`.

**From source** (Rust 1.80+):

```bash
cargo install --path cli
```

You also need `git`, and one way to reach Claude (see *Backends*).

## Usage

```bash
# See what would be read and roughly how many tokens it costs. No Claude calls.
owlmap https://github.com/sinatra/sinatra --dry-run

# Generate the docs (written to owlmap-docs/<owner>-<repo>/)
owlmap https://github.com/sinatra/sinatra

# A local folder works too, and is never modified
owlmap ../my-rails-app --out docs/owlmap
```

Options: `--out DIR`, `--dry-run`, `--backend auto|api|claude-code`,
`--lang en|vi|ja`, `--fresh`,
`--max-files N` (default 500), `--max-input-tokens N` (default 600 000),
`--concurrency N` (default 4, or 2 with Claude Code), `--fast-model ID`,
`--smart-model ID`. Models can also be set with `OWLMAP_FAST_MODEL` /
`OWLMAP_SMART_MODEL`. Run `owlmap --help` for details.

## Re-running is cheap

Run OwlMap again into the same `--out` folder and it only sends what changed:

```
$ owlmap ../my-app --out docs/owlmap
Estimated input: ~12000 tokens (41 of 44 modules unchanged since the last run; ~190000 without cache)
Summarising 3 changed modules with claude-haiku-5-5 (41 unchanged, from cache)…
```

Every Claude call is keyed by a SHA-256 of exactly what would be sent (model,
system prompt, user prompt), stored in `<out>/.owlmap-cache.json`. Editing a
file re-sends only its module; the overview documents are re-written only if a
module summary, the file tree or the manifests changed. An unchanged repository
costs nothing. Changing `--lang` or a model invalidates the affected calls
automatically. Failed calls are never cached, so they are retried next time.
`--fresh` ignores the cache. Commit the cache next to the docs if you want CI
runs to be incremental too.

## Languages

`--lang vi` or `--lang ja` makes Claude write all prose in Vietnamese or
Japanese, and OwlMap's own headings and labels follow. File paths, class,
function and route names are never translated.

## Backends

| `--backend` | Uses | When |
|---|---|---|
| `api` | Claude API with `ANTHROPIC_API_KEY` | Always for the hosted OwlMap service, CI, or anything run for other people |
| `claude-code` | The `claude` CLI on your machine, signed in with your own account | Your own runs while developing and tuning prompts |
| `auto` (default) | `api` if `ANTHROPIC_API_KEY` is set, otherwise `claude-code` | |

The `claude-code` backend runs `claude -p` with no tools, no MCP servers, no
saved session, in an empty temp folder, sending only the prompt OwlMap builds.

**Do not use a Claude subscription to serve other people.** Anthropic's terms
allow Free/Pro/Max logins for ordinary personal use; products and services must
use API keys, and may not route their users' requests through a plan login
([Claude Code legal and compliance](https://code.claude.com/docs/en/legal-and-compliance#authentication-and-credential-use)).

## Output

```
owlmap-docs/<repo>/
├── README.md          # index: documents and a table of modules
├── ARCHITECTURE.md    # summary, stack, components, data flow, Mermaid diagram
├── FLOWS.md           # 2–4 key flows: steps + Mermaid sequence diagrams
├── ONBOARDING.md      # reading order, run locally, where things live, FAQ
├── modules/*.md       # one note per module: purpose, key files, risks…
├── owlmap.json        # run metadata: commit, models, language, token usage, cache reuse, failures
└── .owlmap-cache.json # makes the next run incremental
```

## How it works

| Step | Source file |
|---|---|
| **Fetch** — public GitHub URLs are shallow-cloned into a temp folder (no credentials, symlinks disabled) and deleted afterwards | `src/repo_source.rs` |
| **Scan** — keeps source, config and docs; skips dependencies, build output, lockfiles, binaries, minified files, likely secrets and boilerplate; never follows symlinks | `src/scanner.rs` |
| **Group** — files become modules by folder; large ones are split, tiny ones folded, tests kept apart and read from their first 60 lines | `src/grouper.rs` |
| **Budget** — input size is estimated before any call; the run stops above `--max-input-tokens` | `src/analyzer.rs` |
| **Summarise** — each module goes to the fast model in parallel threads and comes back as JSON; a failed module is reported, not fatal | `src/analyzer.rs` |
| **Synthesise** — the smart model writes the three overview documents from the file tree, manifests and summaries (raw code is not resent) | `src/analyzer.rs`, `src/prompts.rs` |
| **Write** — Markdown files and `owlmap.json` | `src/writer.rs` |

Prompts require every claim to be grounded in the code and anything inferred
to be marked "Unverified:".

## Performance

Local work (scan + grouping), best of 5 runs, release build:

| Repository | Files | Ruby prototype | Rust |
|---|---|---|---|
| sinatra/sinatra | 292 | 148 ms | 8 ms |
| rails/rails | 5 007 | 321 ms | 44 ms |

End-to-end time is still dominated by Claude's responses (seconds to minutes);
raise `--concurrency` to shorten it.

## Development

```bash
cd cli
cargo test            # 30 offline tests: fake Claude client + stub `claude` executable
cargo clippy --all-targets
cargo build --release # target/release/owlmap
```
