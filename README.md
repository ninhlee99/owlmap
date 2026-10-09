# OwlMap

**Turn undocumented codebases into a navigable map.** OwlMap reads a repository with Claude and writes an architecture overview, flow diagrams, per-module notes and an onboarding guide.

> Status: pre-MVP. Landing page is live at https://owlmap.zan.io.vn and the analysis CLI (Rust, single binary) works end to end (tested offline; first live run pending).

## Repository layout

```
owlmap/
├── .claude-plugin/      # This repo is a Claude Code plugin marketplace
├── plugins/owlmap/      # Claude Code plugin: the /owlmap:map skill
├── cli/                 # Analysis core in Rust: repo → Markdown docs via Claude (see cli/README.md)
├── landing/index.html   # Static landing page (EN/VI), live at owlmap.zan.io.vn (Vercel)
├── docs/PLAN.md         # Product, MVP scope, roadmap, business registration, startup program
└── README.md
```

## Use OwlMap inside Claude Code

Install once (the repository is private, so your GitHub account needs access):

```bash
claude plugin marketplace add ninhlee99/owlmap
claude plugin install owlmap@owlmap
```

Then, in any project:

```
/owlmap:map                                   # map the current repository
/owlmap:map ../other-app --out docs/owlmap
/owlmap:map https://github.com/sinatra/sinatra --lang vi
/owlmap:map --module app/models               # refresh one module note
```

Claude Code reads the code with its own tools, delegates module summaries to
parallel subagents on larger repos, and writes the same set of files as the CLI.
It runs on whatever account Claude Code is signed in with. Skill source:
[`plugins/owlmap/skills/map/SKILL.md`](plugins/owlmap/skills/map/SKILL.md).

Without the marketplace: copy `plugins/owlmap/skills/map/` to
`~/.claude/skills/owlmap/`, change `name: map` to `name: owlmap` in its
`SKILL.md`, and run it as `/owlmap`.

## Landing page

A single self-contained HTML file — no build step.

**Preview locally**

```bash
cd landing && python3 -m http.server 8000
# open http://localhost:8000
```

**Configure before going live** — at the top of the `<script>` in `landing/index.html`:

| Constant | What to set |
|---|---|
| `FORM_ENDPOINT` | A form backend URL, e.g. a free [Formspree](https://formspree.io) form (`https://formspree.io/f/xxxx`). While empty, the sign-up form opens an email draft instead. |
| `CONTACT_EMAIL` | Where sign-ups and the footer link go (currently `contact@zan.io.vn`). |

**Where it is deployed**

- Vercel project `owlmap` (team *Ninh Lê's projects*), custom domain **owlmap.zan.io.vn** (CNAME at Cloudflare, verified).
- The project is not yet linked to this repository, so pushing to `main` does **not** redeploy the page. To enable automatic deploys, give the Vercel GitHub app access to `owlmap` (github.com/apps/vercel → Configure), then connect the repository in the Vercel project settings with *Root Directory* `landing`.

## Plan

See [`docs/PLAN.md`](docs/PLAN.md) for the MVP scope, technical approach, 6-week roadmap, costs, Vietnamese company registration checklist and the Claude for Startups application checklist.

## Next milestones

- [ ] Buy domain and set up domain email
- [ ] Connect the waitlist form
- [x] CLI prototype: repo URL → Markdown docs
- [ ] First live run with an API key; tune prompts
- [ ] Quality test set of 5–10 open-source repositories
- [ ] Web MVP and first 5–10 pilot users
