# Orbit Website — Design

**Status:** Draft
**Owner:** Orbit contributors
**Last updated:** 2026-09-07

---

## 1. Purpose & Audience

The Orbit website is a **documentation site**, not a marketing site. It exists to host:

- Reference documentation (CLI commands, activity/job YAML schemas, policy formats)
- How-to guides (task lifecycle, delivery windows, recurring work, task publication)
- Conceptual explainers (activity/job model, task lifecycle, agent runtimes)

**Primary audience:** engineers evaluating or actively using Orbit. They arrive via search, know roughly what they want, and leave as soon as they have it. The site optimizes for that path.

**Non-goals:**
- Lead generation, conversion funnels, email capture
- Blog, changelog-as-narrative, release announcements (those live in `CHANGELOG.md` and git)
- Interactive playgrounds or live demos (revisit if Orbit grows a hosted offering)

---

## 2. Design Principles

1. **Reference-heavy, search-first.** Users land via `⌘K` or Google. Every page must be findable and self-contained.
2. **Minimalism as a feature.** Restraint is the aesthetic. One accent color, one type family per role, no decorative motion in docs content.
3. **Legibility over personality.** The orbit metaphor shows up structurally (logo, section glyphs) — never at the cost of reading comfort.
4. **Static and fast.** Zero JS by default. The homepage's copy controls and its
   narrow-viewport Menu (Escape and breakpoint close) are the scripted exceptions;
   every other interaction is CSS. Hundreds of pages should feel identical in
   performance to ten.
5. **Dark-default, light-available.** Theme toggle persists per user; neither mode is an afterthought.

---

## 3. Visual System

### 3.1 Palette (dark, default)

| Role              | Value       | Notes                                      |
|-------------------|-------------|--------------------------------------------|
| Background        | `#0A0A0A`   | Near-black; avoids pure-black eye strain   |
| Surface           | `#17171A`   | Cards, code blocks, sidebar hover          |
| Border            | `#26262B`   | Structural only; never decorative          |
| Body text         | `#EDEDEF`   | Off-white; softer than `#FFFFFF`           |
| Muted text        | `#9B9BA3`   | Metadata, captions, inactive nav           |
| Accent            | `#6E9FFF`   | Neptune blue; links, active nav, focus     |
| Accent (hover)    | `#8AB3FF`   | One step brighter                          |

Light mode is the same roles inverted; accent stays the same hue, darkened for AA contrast.

### 3.2 Typography

| Role              | Family                         | Size / Line-height      |
|-------------------|--------------------------------|--------------------------|
| Body              | Inter or Geist Sans            | 16px / 1.65              |
| Headings          | Same sans, tighter tracking    | h1 2rem · h2 1.5rem · h3 1.25rem |
| Code (inline/block) | Geist Mono or JetBrains Mono | 14px / 1.6               |
| UI (nav, search)  | Same sans as body              | 14px                     |

No display font. No serif anywhere.

### 3.3 Orbit motif (sparing use)

- **Logo:** a single thin ring with an offset dot. Must be legible at 16px favicon size.
- **Section dividers** in long pages: 1px rule with a small ring glyph centered.
- **Landing page only:** one slow-rotating orbit diagram in the hero. Respects `prefers-reduced-motion`.
- No starfields, parallax, planet illustrations, or animation anywhere inside docs content.

### 3.4 Layout

Three-column, fixed:

```
┌──────────────────────────────────────────────────────┐
│  Logo           Search (⌘K)                      ☾   │
├──────────┬───────────────────────────┬───────────────┤
│  Nav     │  Content (max ~720px)     │  On this page │
│  (left)  │                           │  (right)      │
│          │                           │               │
└──────────┴───────────────────────────┴───────────────┘
```

- Left nav: collapsible sections. Active page marked with a 2px accent bar on the left edge.
- Content column: max-width ~720px, measure 65–75ch for prose.
- Right rail: sticky "On this page" TOC. Muted until the corresponding section is in view.
- Top bar: logo, section links (from 50rem), search, theme toggle. Below 50rem the
  splash header keeps search and exposes section links plus theme through a Menu
  disclosure; documentation pages keep Starlight's sidebar Menu.

### 3.5 Landing page

The homepage uses an in-content hero in place of Starlight's auto-rendered title (which is hidden via a scoped CSS rule on the homepage only):

- **Eyebrow** — mono uppercase tag (`early access`).
- **Headline** — 2.75rem display heading. The only heading on the site that exceeds the body type scale.
- **Lede + install bar + primary/secondary CTAs.** Install bar carries a `$` prompt and a Copy action.
- **Provider strip** — mono uppercase list of the shipped CLI executors, with the
  legacy Gemini executor named in a footnote rather than implied current.
- **Dashboard preview** — a `figure` of the operator Tasks view after that
  default ship: the walkthrough task in `review`, pull request open and
  unmerged, `approve` available, `ship` absent. `role="img"` plus a
  `figcaption` mark it as an illustration of current chrome, not a screenshot
  or a live host. Identifiers are placeholders; no measured counts or
  durations.

Below the hero, in order:

1. **One task, one pull request** — a 4-card grid for create → ship → inspect
   → review. Each card carries a mono numbered tag `01`–`04` and the command
   it runs. New tasks start in `proposed` until approved into the backlog.
   The default path stops in `review` with the PR unmerged; approving the
   task does not merge the pull request. A sentence under the grid points at
   Install and First Task.
2. **Other delivery modes** — a four-mode explorer over `orbit run ship`,
   `--mode local`, `orbit run auto` and `orbit run ship-sweep`. Each panel
   states the command, where the run stops, and that `--complete` is a separate
   explicit authorization. Built as a native radio group switched by CSS
   `:has()`, so pointer, keyboard and screen-reader support are the platform's
   and the selected panel still renders without JavaScript. This section stays
   after the walkthrough so the default path is read first.
3. **Why Orbit** — a 4-card value-prop strip. Each card carries a thin SVG glyph;
   these and the walkthrough tags are the only glyphs in content.
4. **Go further** — a 4-card grid routing to continuous delivery, recurring work,
   publication and recovery, and the CLI reference.
5. **Explore the docs** — a flat index of the sidebar groups.

Commands shown on this page must match current CLI behaviour, and illustrative
output must say that it is illustrative. The page advertises no unlanded feature
and publishes no live metric.

Scripts are limited to the copy controls and the homepage Menu's Escape /
breakpoint close. Copy buttons are served `hidden` and unhidden by that script,
so a page without JavaScript shows the command text and no dead control; a
clipboard that is
unavailable or refuses the write reports failure rather than a false success.

Other pages keep Starlight's default chrome (auto title, sidebar, TOC) unchanged.

---

## 4. Information Architecture

Initial top-level sections (left nav, in order):

1. **Introduction** — what Orbit is, who it's for, 2-minute read
2. **Getting Started** — install, first task, activity catalog
3. **Concepts** — tasks, activities/jobs, policies, agents
4. **How-to Guides** — task-oriented recipes
5. **Reference** — CLI, YAML schemas, config, scoping rules
6. **Contributing** — local dev, crate layout, PR workflow

Each section has an index page that lists its children with one-line descriptions. No "coming soon" placeholders — sections appear only when populated.

---

## 5. Tech Stack

- **Framework:** [Astro Starlight](https://starlight.astro.build)
- **Search:** Pagefind (built into Starlight, static, offline, no third-party account)
- **Content:** MDX in `src/content/docs/`
- **Styling:** Starlight's CSS custom properties, overridden in a single `custom.css`
- **Hosting:** The public edge and DNS are on Cloudflare. The repository-supported
  path directly uploads to the existing Pages project identified by the protected
  production environment, and publishes only from the release/production `main`
  branch. The `orbit-cli.com` DNS remains externally managed; publication neither
  provisions hosting nor edits DNS. Cloudflare Pages applies the repository-owned
  `public/_headers` policy to HTTPS responses; the externally managed Cloudflare
  zone owns HTTP-to-HTTPS redirection. See ORB-11379.
- **Repo layout:** new top-level `website/` directory, independent of the Rust workspace

### 5.1 Why Starlight over Nextra

- Docs-first defaults map 1:1 to this site's stated values
- Zero JS by default → consistent perf as the site grows
- Pagefind search is excellent and fully static
- Less framework surface to fight when enforcing minimalism

Nextra is reserved for a future scenario where interactive React widgets become core content (API explorers, config builders). Not a concern at launch.

---

## 6. Content Conventions

- **Page frontmatter:** `title`, `description` required; `sidebar.order` optional.
- **Headings:** start at `h2` within content (Starlight renders `h1` from frontmatter).
- **Code blocks:** always language-tagged. Long examples collapsible.
- **Cross-links:** relative paths only; no hardcoded domains.
- **Internal references:** do not publish repository-internal artifact identifiers.
- **Voice:** terse, declarative, second-person ("you run", not "the user runs"). No marketing adjectives.

---

## 7. Open Questions

1. **Versioning.** Starlight supports versioned docs via directory structure. Add it when release-specific documentation becomes necessary.
2. **Architecture detail.** Crate boundaries and dependency direction are contributor material, not published here; they live in the repository's `ARCHITECTURE.md`. Revisit only if a public extension surface makes them user-facing.
3. **Logo design.** Ring-with-offset-dot concept agreed; actual SVG not yet drawn.
4. **Analytics.** Plausible (privacy-respecting) or none at all? Default to none unless there's a decision to measure something specific.

---

## 8. Out of Scope (explicitly)

- Interactive code playgrounds
- Authenticated / gated content
- Localization (revisit if Orbit gains non-English contributors at scale)
- Comments, discussions, or embedded social
- A blog

---

## 9. References

- [Radix Primitives docs](https://www.radix-ui.com/primitives/docs) — primary visual reference
- [Astro Starlight](https://starlight.astro.build) — framework docs
- [Tailwind docs](https://tailwindcss.com/docs) — information density reference
- [Pagefind](https://pagefind.app) — search implementation
