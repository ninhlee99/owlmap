# OwlMap

**Turn undocumented codebases into a navigable map.** OwlMap reads a repository with Claude and writes an architecture overview, flow diagrams, per-module notes and an onboarding guide.

> Status: pre-MVP. Landing page is live at https://owlmap-ninh-le-projects.vercel.app and the analysis CLI works end to end (tested offline; first live API run pending).

## Repository layout

```
owlmap/
├── cli/                 # Analysis core: repo → Markdown docs via the Claude API (see cli/README.md)
├── landing/index.html   # Static landing page (EN/VI), deployed on Vercel
├── docs/PLAN.md         # Product, MVP scope, roadmap, business registration, startup program
└── README.md
```

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
| `CONTACT_EMAIL` | Your domain email once it exists (currently the placeholder `hello@owlmap.dev`). |

**Deploy with GitHub Pages**

1. Repository → *Settings* → *Pages* → *Source*: **GitHub Actions**.
2. Push to `main`. The included workflow `.github/workflows/pages.yml` publishes the `landing/` folder.
3. To use your own domain, add it under *Custom domain* and create the DNS record your registrar shows.

> GitHub Pages on a **private** repository needs a paid GitHub plan. Alternatives: make the repo public, or deploy `landing/` on Vercel/Netlify/Cloudflare Pages (free tiers support private repos).

## Plan

See [`docs/PLAN.md`](docs/PLAN.md) for the MVP scope, technical approach, 6-week roadmap, costs, Vietnamese company registration checklist and the Claude for Startups application checklist.

## Next milestones

- [ ] Buy domain and set up domain email
- [ ] Connect the waitlist form
- [x] CLI prototype: repo URL → Markdown docs
- [ ] First live run with an API key; tune prompts
- [ ] Quality test set of 5–10 open-source repositories
- [ ] Web MVP and first 5–10 pilot users
