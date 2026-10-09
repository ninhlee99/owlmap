---
name: map
description: Generate OwlMap documentation for one or more codebases — architecture overview, key flows with Mermaid diagrams, one note per module, an onboarding guide, and for several repositories a SYSTEM.md showing how they connect — written as Markdown files. Use when the user runs /owlmap:map or asks to map, document or onboard onto whole repositories, including large ones.
argument-hint: "[path|github-url]... [--out DIR] [--lang en|vi|ja] [--include GLOB] [--exclude GLOB] [--skip-tests] [--detail quick|standard|deep]"
disable-model-invocation: true
allowed-tools: Read Glob Grep Bash(owlmap *) Bash(git ls-files *) Bash(git -C * ls-files *) Bash(git -C * rev-parse *) Bash(tail *)
---

# OwlMap: map one or more codebases

Arguments: `$ARGUMENTS`

Targets are local folders or public `https://github.com/owner/repo` URLs.
No target means the current working directory.

- **Current folder is a project holding several repositories** (for example
  `candidate/ company/ api/` side by side, the folder itself not a git repo):
  `owlmap` finds them all by itself and writes everything to **`./owlmap/`**:
  `owlmap/README.md`, `owlmap/SYSTEM.md`, and `owlmap/<repo>/` for each
  repository. A shared `docker-compose.yml`, `Makefile` or `README.md` in the
  project folder is used as evidence for how they connect. Run `owlmap` with no
  target and no `--out`, unless the user asks otherwise.
- **Current folder is one repository:** docs go to `owlmap-docs/<repo>/`.
- **Several targets given** (for example `../candidate ../company ../api`): one
  folder of docs per repository plus a `SYSTEM.md`.

**Never read whole repositories into this conversation.** Large codebases do
not fit, and a conversation cannot resume after a limit. The work belongs in
the `owlmap` command, which gives every module its own fresh Claude call, saves
progress as it goes, and resumes after an interruption.

## 1. Use the `owlmap` command (preferred)

Run `owlmap --version`.

**If it is installed**, follow these steps:

1. **Estimate first.** Run the dry run with the user's targets (none for the
   current folder) and options:
   `owlmap [targets…] --dry-run [--out DIR] [--lang …] [--include …] [--exclude …] [--skip-tests] [--detail …]`
   If it prints "Found N repositories in .", say which ones it found and where
   the docs will go (`./owlmap/`). Show the user, in a short table, each
   repository's files, modules and estimated input tokens, plus the total.
2. **Confirm when it is big.** If the total is over 1,000,000 tokens, say that
   the run uses their own Claude account's usage and can take a long time, and
   offer ways to make it smaller before starting:
   - `--exclude 'dir/**'` for folders that don't matter (vendored plugins, generated clients)
   - `--skip-tests`
   - `--detail quick`
   - `--include 'app/**'` to focus on one part

   Wait for their answer. Under 1,000,000 tokens, go straight on.
3. **Run it in the background** (a long run outlasts a single command's
   timeout), writing a log:
   `owlmap [targets…] --backend claude-code [same options] > owlmap.log 2>&1`
   Start it as a background command, then check `tail -n 20 owlmap.log` from
   time to time and tell the user how far it has got (modules done out of the
   total, per repository). Don't flood the conversation with every log line.
4. **When it ends**, read the exit status and the end of the log:
   - **0, finished:** tell the user where the docs are (`./owlmap/README.md`
     and `./owlmap/SYSTEM.md` for a project folder; otherwise `README.md` in the
     output folder), how many calls were reused from
     the cache, and suggest what to read first. Open `README.md` (and
     `SYSTEM.md`) and spot-check five file or class names against the code
     with Grep. Report any that don't exist.
   - **2, stopped:** usually the account's usage limit. Everything finished so
     far is saved. Tell the user to run the same command again later, and that
     it will continue where it stopped.
   - **anything else:** show the error line and suggest the fix the message
     gives (for example `--max-input-tokens`, or a narrower `--include`).

Running again later into the same `--out` only sends what changed.

**If it is not installed**, tell the user it is a one-time install:
- From the source repository (needs Rust and access to the private repo):
  `cargo install --git https://github.com/ninhlee99/owlmap owlmap`
- or download a prebuilt binary from the repository's Releases page.

Then continue with step 1 once it is installed. If they don't want to install
it, use the fallback below, but only for small targets.

## 2. Fallback without the command (small repositories only)

Use this only when the targets together have **at most 300 source files**
(count with `git ls-files`, excluding dependencies, tests, migrations and
translations). For anything larger, explain why the command is needed and stop.

Work entirely on disk so the conversation never holds all the summaries:

1. For each target, list the files worth reading (as `owlmap` would): skip
   `node_modules/ vendor/ dist/ build/ tmp/ log/ coverage/`, lockfiles, minified
   files, binaries, `db/migrate/`, locale files, fixtures and generated code.
   **Never open `.env*`, `*.pem`, `*.key`, `*credentials*`**, and never copy a
   secret value into the docs.
2. Group the files into modules by folder (`app/models`, `app/controllers`,
   `lib/foo`, `(root)`; tests separately). Write the plan to
   `<out>/.owlmap/plan.json`.
3. Launch read-only subagents (Explore type), a few modules each. Give each one
   its modules' exact file lists, the JSON shape below, and these rules: ground
   every claim in the code and prefix guesses with "Unverified:". Each subagent
   writes one file per module to `<out>/.owlmap/summaries/<repo>/<slug>.json`
   and returns only "done".

   ```json
   {"module": "app/models", "slug": "app-models", "purpose": "1-2 sentences",
    "key_files": [{"path": "…", "role": "…"}], "public_interface": [], "depends_on": [],
    "used_by": [], "data": [], "risks": [], "notes": ""}
   ```

   Skip any module whose summary file already exists. That makes this resumable.
4. Write the documents from the summary files using
   [templates.md](templates.md): per repository `README.md`, `ARCHITECTURE.md`,
   `FLOWS.md`, `ONBOARDING.md` and `modules/<slug>.md`. For several
   repositories, also write `SYSTEM.md`: repositories table, how they connect
   (with evidence such as environment variables like `*_API_URL`, URLs and
   client classes, found with Grep), a Mermaid diagram, and cross-repository
   flows.
5. Spot-check ten names with Grep and fix any that don't exist. Check that every
   Mermaid block parses.

Write prose in the requested `--lang` (`en` default, `vi`, `ja`). File, class,
function and route names always stay exactly as in the code.
