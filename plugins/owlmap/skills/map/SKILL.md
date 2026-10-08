---
name: map
description: Generate OwlMap documentation for a codebase — architecture overview, key flows with Mermaid diagrams, one note per module and an onboarding guide — written as Markdown files. Use when the user runs /owlmap:map or asks to map, document or onboard onto a whole repository.
argument-hint: "[path | github-url] [--out DIR] [--lang en|vi|ja] [--module NAME]"
disable-model-invocation: true
allowed-tools: Read Glob Grep Bash(git ls-files *) Bash(git -C * ls-files *) Bash(git rev-parse *) Bash(git -C * rev-parse *) Bash(git clone --depth 1 *)
---

# OwlMap: map a codebase

Arguments: `$ARGUMENTS`

Produce documentation a developer new to this codebase can trust. Every
statement must be grounded in code you actually read; anything inferred is
marked `Unverified:`. Wrong documentation is worse than none.

## 1. Resolve the target and options

Parse the arguments:

- **Target** (optional): a local folder, or a public `https://github.com/owner/repo` URL.
  Default: the current working directory.
  For a GitHub URL, shallow-clone it into a new temporary folder with
  `git clone --depth 1 -- <url> <tmp>/repo` and work there. Never ask for credentials.
- `--out DIR`: where to write. Default `owlmap-docs/` at the root of the current
  working directory (for a cloned URL: `owlmap-docs/<owner>-<repo>/`).
- `--lang CODE`: language of the prose (`en` default, `vi` Vietnamese, `ja` Japanese).
  File, class, function and route names always stay exactly as in the code.
- `--module NAME`: only (re)write `modules/<slug>.md` for that module and leave
  every other file untouched.

If `--out` already contains an `owlmap.json`, read it first: tell the user which
commit it was generated from and that you will overwrite it.

## 2. List the files worth reading

Prefer `git ls-files` inside the target (it respects .gitignore); fall back to Glob.
Then drop:

- dependencies and build output: `node_modules/ vendor/ dist/ build/ out/ target/ .next/ coverage/ tmp/ log/ public/assets/ __pycache__/ .venv/`
- lockfiles (`*.lock`, `package-lock.json`, `yarn.lock`, `pnpm-lock.yaml`, `go.sum`…), minified files (`*.min.js`, `*.min.css`), images, fonts, archives and other binaries
- boilerplate: `CHANGELOG*`, `HISTORY*`, `LICENSE*`, `CODE_OF_CONDUCT*`, `CONTRIBUTING*`, `SECURITY*`
- **anything that may hold secrets**: `.env*`, `*.pem`, `*.key`, `id_rsa*`, `*credentials*`, `*.p12`. Never open these, and never copy a secret value into the docs even if you see one elsewhere — write "a secret is configured in <file>" instead.
- files over ~100 KB (note them as skipped)

If more than 1,500 files remain, stop and ask the user to point at a subfolder.

## 3. Group files into modules

A module is normally a folder. Rules (same as the OwlMap CLI):

- Conventional containers are split one level deeper: `app/ src/ lib/ packages/ apps/ internal/ pkg/ cmd/ services/ modules/ components/ features/` → `app/models`, `app/controllers`, `src/api`…
- A module larger than ~120 KB of text is split by the next folder level, or into `(part N)` chunks.
- Tiny modules (under ~3 KB) are folded into their parent folder; tiny top-level folders are batched as `(small folders)`.
- Tests (`test/ spec/ __tests__/ e2e/`, `*_test.*`, `*_spec.*`, `*.test.*`, `*.spec.*`) form separate `<area> (tests)` modules. Read only their first ~60 lines.
- Root-level files form `(root)`.

Each module's slug is its name lower-cased with runs of non-alphanumerics turned into `-` (`app/models` → `app-models`, `(root)` → `root`).

Show the user the module list (name, file count) in one short table before continuing.

## 4. Summarise each module

For every module, read its files and build this summary (keep it in your
working notes; do not write it to disk yet):

```json
{
  "module": "app/models",
  "slug": "app-models",
  "purpose": "1-2 sentences",
  "key_files": [{"path": "app/models/user.rb", "role": "one line"}],
  "public_interface": ["entry points other code calls"],
  "depends_on": ["modules, packages, external services"],
  "used_by": ["callers you saw"],
  "data": ["tables, models, queues, caches, files read or written"],
  "risks": ["what is easy to break when changing this, and why"],
  "notes": ""
}
```

At most 12 `key_files`. Use empty arrays rather than guessing.

**With more than 6 modules, delegate.** Launch read-only subagents (Explore
type) in parallel, a few modules each, and give every subagent: the module
names with their exact file lists, the JSON shape above, the grounding rule,
the secrets rule, and "return only the JSON array". Do the synthesis (step 5)
yourself from what they return.

## 5. Write the documents

Use the templates in [templates.md](templates.md). Write, in `--out`:

| File | Contents |
|---|---|
| `README.md` | Index: what was mapped (path or URL, commit from `git rev-parse HEAD`), links to the three documents, and a table of modules with file count and first sentence of purpose |
| `ARCHITECTURE.md` | Summary, tech stack, components, how data moves, external services, one Mermaid `flowchart LR` (≤15 nodes) |
| `FLOWS.md` | 2–4 key flows: numbered steps naming real files/classes, a Mermaid `sequenceDiagram` (≤8 participants), "Watch out" pitfalls |
| `ONBOARDING.md` | Read-these-first list, run-it-locally commands taken only from files you saw, where-things-live table, newcomer Q&A, handle-with-care |
| `modules/<slug>.md` | One per module, from its summary |
| `owlmap.json` | `{"generator": "owlmap-skill", "version": "0.1.0", "generated_at", "source": {"path" or "url", "commit"}, "lang", "files", "modules": [names], "skipped": {reason: count}}` |

Link module names as `[name](modules/<slug>.md)`.

## 6. Check before you finish

- Every file, class, function and route named in the docs exists — spot-check
  at least 10 names with Grep and fix any that don't.
- Every Mermaid block is valid: `flowchart LR` / `sequenceDiagram` first line,
  node ids without spaces, labels with special characters wrapped in quotes.
- No secret values anywhere in the output.
- If you cloned a URL, delete the temporary folder.

Finish with: where the docs were written, the module count, anything skipped
or marked `Unverified:`, and one suggestion for what to read first.
