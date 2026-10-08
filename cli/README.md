# OwlMap CLI

The analysis core of OwlMap: point it at a repository and it writes an
architecture overview, key flows with Mermaid diagrams, per-module notes and an
onboarding guide, using the Claude API.

Pure Ruby standard library — no gems to install.

## Requirements

- Ruby 3.2+
- `git`
- A Claude API key from the [Claude Console](https://platform.claude.com/), exported as `ANTHROPIC_API_KEY`

## Usage

```bash
export ANTHROPIC_API_KEY=sk-ant-...

# See what would be read and roughly how many tokens it costs. No API calls.
cli/bin/owlmap https://github.com/sinatra/sinatra --dry-run

# Generate the docs (written to owlmap-docs/<owner>-<repo>/)
cli/bin/owlmap https://github.com/sinatra/sinatra

# A local folder works too, and is never modified
cli/bin/owlmap ../my-rails-app --out docs/owlmap
```

Options: `--out DIR`, `--dry-run`, `--max-files N` (default 500),
`--max-input-tokens N` (default 600 000), `--concurrency N` (default 4),
`--fast-model ID`, `--smart-model ID`. Models can also be set with
`OWLMAP_FAST_MODEL` / `OWLMAP_SMART_MODEL`.

## Output

```
owlmap-docs/<repo>/
├── README.md          # index: documents and a table of modules
├── ARCHITECTURE.md    # summary, stack, components, data flow, Mermaid diagram
├── FLOWS.md           # 2–4 key flows: steps + Mermaid sequence diagrams
├── ONBOARDING.md      # reading order, run locally, where things live, FAQ
├── modules/*.md       # one note per module: purpose, key files, risks…
└── owlmap.json        # run metadata: commit, models, token usage, failures
```

## How it works

1. **Fetch** — public GitHub URLs are shallow-cloned into a temp folder (no
   credentials, symlinks disabled) and deleted afterwards.
2. **Scan** — keeps source, config and docs; skips dependencies, build output,
   lockfiles, binaries, minified files, likely secrets (`.env`, keys) and
   boilerplate (CHANGELOG, LICENSE…). Symlinks are never followed.
3. **Group** — files become modules by folder (`app/models`, `lib/foo`…).
   Large modules are split, tiny ones folded together, and tests are kept
   apart and read from their first 60 lines only.
4. **Budget** — the input size is estimated before any API call; the run stops
   if it exceeds `--max-input-tokens`.
5. **Summarise** — each module goes to the fast model in parallel and comes
   back as structured JSON. A failed module is reported, not fatal.
6. **Synthesise** — the smart model writes the three overview documents from
   the file tree, manifests and module summaries (raw code is not resent).

Prompts live in `lib/owlmap/prompts.rb`. They require every claim to be
grounded in the code and anything inferred to be marked "Unverified:".

## Tests

```bash
ruby -Icli/lib cli/test/owlmap_test.rb
```

The suite uses a fake Claude client, so it runs offline and costs nothing.
