# OwlMap CLI

The analysis core of OwlMap, written in Rust: point it at one or more
repositories and it writes an architecture overview, key flows with Mermaid
diagrams, per-module notes and an onboarding guide for each, plus a system
overview of how they connect, using Claude. Built for large codebases: it reads
selectively, summarises in tiers, and resumes after interruptions.

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

# Several repositories that form one system → one folder each + SYSTEM.md
owlmap ../candidate ../company ../api --out docs/system --lang vi

# From a project folder that holds them (candidate/ company/ api/ side by side):
# finds every repository and writes ./owlmap/
cd ~/work/project && owlmap --lang vi

# A very large monorepo, narrowed
owlmap ../big-app --exclude 'plugins/**' --skip-tests --detail quick --dry-run
```

Options: `--out DIR`, `--dry-run`, `--backend auto|api|claude-code`,
`--lang en|vi|ja`, `--fresh`, `--detail quick|standard|deep`,
`--include GLOB`, `--exclude GLOB` (both repeatable), `--skip-tests`,
`--all-files`, `--max-files N` (default 20 000 per repository),
`--max-input-tokens N` (default 20 000 000 for the whole run),
`--concurrency N` (default 4, or 2 with Claude Code), `--fast-model ID`,
`--smart-model ID`, `--verbose`. Models can also be set with `OWLMAP_FAST_MODEL` /
`OWLMAP_SMART_MODEL`. Run `owlmap --help` for details.

## Several repositories

Give several targets and OwlMap documents each one, then writes `SYSTEM.md`:
what each repository is, **how they connect** (who calls whom, over what, with
the evidence), a diagram, end-to-end flows that cross repositories, and shared
concerns such as authentication and configuration.

```
docs/system/
├── README.md        # workspace index
├── SYSTEM.md        # how the repositories work together
├── candidate/       # full doc set for each repository
├── company/
└── api/
```

Run `owlmap` with no target inside a **project folder** (not itself a git
repository) and it expands into every git repository one level down, skipping
hidden folders and its own `owlmap/` output, and writes to `./owlmap/`.

The connections are grounded in evidence OwlMap extracts from the code without
Claude:
- the environment variables each repository reads (`ENV['API_BASE_URL']`,
  `process.env.…`, `${…}` in config);
- the hosts it calls;
- references to the other repositories **as services**: `company_api`,
  `CANDIDATE_URL`, `http://api:3000`, `../company`. A bare word such as a
  `Company` model is not counted, since repository names are often domain nouns;
- shared `docker-compose.yml`, `Makefile` and `README.md` in the project folder.

Secret values seen in that evidence are never reproduced in the docs.

## Large repositories

| Measure | What it does |
|---|---|
| Skip bulk | Migrations, translation files, fixtures, snapshots, generated code and `public/` are skipped by default (`--all-files` keeps them) |
| `--detail standard` (default) | Files over ~150 lines are read as an outline: the first 40 lines plus every declaration with its line number. Tests are read as their test names |
| `--detail quick` / `deep` | Outlines for almost everything / full files up to 400 lines |
| Areas | Above 30 modules, module summaries are rolled up into areas (folders, at most 40 modules each, tiny ones merged) before the overview, so no call has to read hundreds of summaries |
| Compact overview input | The overview documents read trimmed summaries and, for big repositories, folder counts instead of every path |
| Narrowing | `--include`, `--exclude`, `--skip-tests` |

Measured on three large open-source Rails codebases with `--dry-run`:

| Repository | Files kept | Before (v0.3) | Now, `standard` | Now, `quick` |
|---|---|---|---|---|
| rails/rails | 3 676 | ~4.2M tokens (refused: over 500 files) | ~1.3M | ~0.8M |
| mastodon/mastodon | 4 312 | ~5.4M (refused) | ~1.6M | ~0.9M |
| discourse/discourse | 17 356 | ~17.8M (refused) | ~6.3M | ~3.9M |

An end-to-end run of all three together (through a stub Claude) made 948 calls;
the largest single prompt was about 49k tokens, well within every model's context.

## Interruptions and usage limits

Progress is saved to `<out>/.owlmap-cache.json` every few calls. If Claude
reports a usage limit, a sign-in problem or a bad key, OwlMap stops starting new
calls immediately, writes the docs for repositories that finished, and exits
with code **2**. Run the same command again later: it continues where it
stopped. Transient errors are retried automatically.

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
├── owlmap.json        # run metadata: commit, models, language, areas, token usage, cache reuse, failures
└── .owlmap-cache.json # makes the next run incremental and resumable
```

## How it works

| Step | Source file |
|---|---|
| **Fetch** — public GitHub URLs are shallow-cloned into a temp folder (no credentials, symlinks disabled) and deleted afterwards | `src/repo_source.rs` |
| **Scan** — keeps source, config and docs; skips dependencies, build output, lockfiles, binaries, minified files, likely secrets and boilerplate; never follows symlinks | `src/scanner.rs` |
| **Group** — files become modules by folder; large ones are split, tiny ones folded, tests kept apart | `src/grouper.rs` |
| **Roll up** — above 30 modules, summaries are grouped into areas and summarised again | `src/analyzer.rs` |
| **Connect** — for several repositories, integration evidence is extracted and SYSTEM.md written | `src/workspace.rs` |
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
cargo test            # 38 offline tests: fake Claude client + stub `claude` executable
cargo clippy --all-targets
cargo build --release # target/release/owlmap
```
