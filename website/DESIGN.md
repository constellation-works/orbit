# Orbit Website — Design

**Status:** Draft
**Owner:** Orbit contributors
**Last updated:** 2026-09-17

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
- **Headline** — 3.6rem display heading (2.75rem on narrower desktops, 2.2rem
  on phones). The only heading on the site that exceeds the body type scale.
- **Lede + install bar + primary/secondary CTAs.** Install bar carries a `$` prompt and a Copy action.
- **Provider strip** — the shipped CLI executors as a plain list under a
  hairline, with the legacy Gemini executor named in a footnote rather than
  implied current.
- **Session preview** — a `figure` of one exchange between the reader, their
  agent, and Orbit, laid out as a conversation with receipts. Turns are
  body type with a mono speaker label (`you` in the accent, `agent` in
  grey). Under each agent turn a receipt block, indented to the text column,
  lists that turn's tool calls one per mono row — tool name, arrow, what
  Orbit returned — with task statuses drawn as pills: `orbit.task.add`
  (task in `proposed`); the agent asks for the go-ahead and the reader gives
  it; `orbit.task.update` (`proposed → backlog`, the approval) and
  `orbit.workflow.ship` (run ID, scope reserved, worktree isolated);
  `orbit.workflow.run.show` (steps settled, PR opened, task in `review`); the
  agent reports the PR is open and the diff and merge are the reader's. The
  agent drives Orbit over MCP, so the conversation is the hero visual and the
  CLI is plumbing. Arguments are omitted so the calls read as one line each;
  tool names are real and identifiers are placeholders, and the `figcaption`
  says so. `role="img"` marks it illustrative, not captured output.

Below the hero, each section opens on a two-column head — mono eyebrow and a
one-sentence heading on the left, a short lede on the right — and in order:

1. **One conversation, one pull request** — the task lifecycle as a rail
   (`proposed → backlog → in-progress → review → done`, with the default
   ship's stop at `review` marked and `done` dashed), then a 4-card grid for
   say what you want → the agent files it → you say go, it ships → you
   review the pull request. Each card carries a mono numbered tag `01`–`04`
   and the command or MCP call behind it; the review card is outlined in the
   accent because that is where the default path stops. New tasks start in
   `proposed` until approved into the backlog; the same `--approve` later
   takes `review` to `done`, and neither step merges the pull request. A
   sentence under the grid points at Install and Set Up MCP, with First Task
   as the by-hand CLI route.
2. **When you are not in the loop** — one table over `orbit run ship`,
   `--mode local`, `orbit run auto`, a scoped `orbit operation` grant, and
   `orbit run ship-sweep`: the command, where the run stops, and that
   completing delivery is a separate explicit authorization, side by side so
   modes compare without clicking. Row headers link to each mode's guide.
   This section stays after the walkthrough so the attended path is read
   first.
3. **Why Orbit** — a 2×2 value-prop grid. Each card carries a thin SVG glyph
   beside its copy and the command that shows the property; these and the
   walkthrough tags are the only glyphs in content.
4. **Go further** — a 3-card grid routing to continuous delivery, recurring
   work, and publication and recovery, with the CLI reference linked from the
   section head.
5. **Explore the docs** — a flat five-column index of the sidebar groups,
   closing the page in one bordered panel.

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
- **Hosting:** The public edge and DNS are on Cloudflare. Daniel manually
  publishes the static output; the `orbit-cli.com` DNS remains externally
  managed, and this repository neither provisions hosting nor edits DNS.
  Cloudflare Pages applies the repository-owned `public/_headers` policy to
  HTTPS responses; the externally managed Cloudflare zone owns HTTP-to-HTTPS
  redirection.
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
