# Orbit Website — Design

The design contract for `orbit-cli.com`: what the site is for, how it looks, how it is organized and how its pages are written. Read it before changing the site's look, navigation or homepage.

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
4. **Static and fast.** Content is rendered at build time. Small browser scripts
   support search, the theme toggle, and the homepage's narrow-viewport Menu;
   they should not delay the page's readable state.
5. **Dark-default, light-available.** First-time visitors see dark whatever their OS prefers; the theme toggle persists an explicit choice, and a stored light choice wins. Neither mode is an afterthought.

Visual reference: the Radix Primitives docs. Density reference: the Tailwind docs.

---

## 3. Visual System

### 3.1 Palette (dark, default)

| Role              | Value       | Notes                                      |
|-------------------|-------------|--------------------------------------------|
| Background        | `#0A0A0A`   | Reading surface; avoids pure-black eye strain |
| Surface           | `#17171A`   | Cards, code blocks, sidebar hover          |
| Border            | `#26262B`   | Structural only; never decorative          |
| Body text         | `#EDEDF0`   | High contrast on the reading surface       |
| Muted text        | `#9B9BA3`   | Metadata, captions, inactive nav           |
| Accent            | `#6E9FFF`   | Links, active nav, focus                   |
| Accent (hover)    | `#8AB3FF`   | One step brighter                          |

Light mode keeps the same roles on a `#FFFFFF` background with `#17171A` body
text and a darker `#2F67D8` accent for AA contrast. `:root` in `custom.css`
carries the dark tokens, so a page renders dark even without JavaScript;
`:root[data-theme='light']` overrides them. The inline script in
`ThemeProvider.astro` sets `data-theme` before first paint: light only when the
visitor's stored choice (`orbit-theme-choice`) is `light`, dark otherwise,
regardless of `prefers-color-scheme`. A plain page load does not save a theme
preference; the toggle saves an explicit light or dark choice.

### 3.2 Typography

| Role              | Family                         | Size / Line-height      |
|-------------------|--------------------------------|--------------------------|
| Body              | Geist (self-hosted)            | 16px / 1.65              |
| Headings          | Same sans, tighter tracking    | h1 2rem · h2 1.5rem · h3 1.25rem |
| Code (inline/block) | Geist Mono (self-hosted)     | 14px / 1.6               |
| UI (nav, search)  | Same sans as body              | 14px                     |

No display font. No serif anywhere. Both families ship as npm packages
(`@fontsource-variable/geist`, `@fontsource-variable/geist-mono`) listed in
`customCss`, so the site loads no fonts from other domains.

### 3.3 Orbit motif (sparing use)

- **Logo:** a single thin ring with an offset dot. Must be legible at 16px favicon size.
- **Section dividers** in long pages: 1px rule with a small ring glyph centered.
- **Landing page:** the hero carries one demo video, a tour of the real dashboard (see 3.5), not an orbit diagram. It is the only motion on the site and stops for readers who ask for reduced motion.
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
- Top bar: logo, latest-release badge (read from `CHANGELOG.md` at build time),
  section links including Changelog (from 50rem), search, theme toggle. Below 50rem the
  splash header keeps search and exposes section links plus theme through a Menu
  disclosure; documentation pages keep Starlight's sidebar Menu.

### 3.5 Landing page

The homepage replaces Starlight's auto-rendered title with an in-content hero;
a CSS rule scoped to the homepage hides the title. Other pages keep Starlight's
default chrome (auto title, sidebar, TOC).

- **Hero:** a release chip linking to the changelog, the headline, a lede that
  says what Orbit is, the CTAs, a requirements note and the shipped CLI
  executors as a plain list. A legacy executor is footnoted, never implied
  current.
- **Hero video:** the site's only motion, a muted, looping tour of the real
  dashboard. It stays on its poster frame under
  `prefers-reduced-motion: reduce`, its caption dates the capture so its counts
  read as a snapshot, and its `aria-label` narrates it. The source lives in
  `marketing/media/video/orbit-dashboard-tour`; the site serves a web re-encode
  from `public/media/`.
- **Below the hero:** short sections that take a reader from what Orbit
  promises, to how it works and runs unattended, to where to go next. Each
  opens on a two-column head: a mono eyebrow and one-sentence heading, then a
  short lede. The page ends on a Quickstart whose copy buttons copy exactly
  the command shown. The footer, not the page, carries the full docs index.

Rules for the page:

- Commands shown match current CLI behaviour, and illustrative output says it
  is illustrative.
- Advertise no unlanded feature and publish no live metric; captured numbers
  carry their capture date.
- Scripts stay small and serve only the page's documented interactions: the
  shared theme script, the Menu's Escape and breakpoint close, the copy
  buttons, and pausing the hero video under reduced motion. Nothing delays the
  readable state.

---

## 4. Information Architecture

Top-level sections (left nav, in order), following the reader from setup to
lookup:

1. **Start Here** — what Orbit is, quickstart, install, connect your agent
   (MCP), first task, delivery workflows
2. **Concepts** — tasks, agents and crews, activities and jobs, routines and
   auto-tasks, policies
3. **Guides** — task-oriented recipes for everyday use
4. **Operate** — backup and restore, multi-machine drains
5. **Reference** — CLI, configuration, YAML schemas, policy format, scoping
6. **Project** — changelog, contributing (collapsed), privacy

Sidebar labels match page titles (section indexes read "Overview"). Moving a page between groups never moves
its URL.

Each section has an index page that lists its children with one-line descriptions. No "coming soon" placeholders — sections appear only when populated.

---

## 5. Tech Stack

- **Framework:** [Astro Starlight](https://starlight.astro.build)
- **Search:** Pagefind (built into Starlight, static, offline, no third-party account)
- **Content:** Markdown (`.md`) in `src/content/docs/`
- **Styling:** Starlight's CSS custom properties, overridden in a single `custom.css`
- **Hosting:** The public edge and DNS are on Cloudflare. Daniel manually
  publishes the static output; the `orbit-cli.com` DNS remains externally
  managed, and this repository neither provisions hosting nor edits DNS.
  Cloudflare Pages applies the repository-owned `public/_headers` policy to
  HTTPS responses; the externally managed Cloudflare zone owns HTTP-to-HTTPS
  redirection.
- **Repo layout:** `website/`, independent of the Rust workspace

---

## 6. Content Conventions

- **Page frontmatter:** `title`, `description` required; `sidebar.order` optional.
- **Headings:** start at `h2` within content (Starlight renders `h1` from frontmatter).
- **Code blocks:** always language-tagged. Long examples collapsible.
- **Cross-links:** relative paths only; no hardcoded domains.
- **Internal references:** do not publish repository-internal artifact identifiers in docs pages. The one exception is the changelog page: it renders `CHANGELOG.md` as-is, and its bracketed task IDs stay because they are the pointer from a release note to the pull request that delivered it.
- **Voice:** terse, declarative, second-person ("you run", not "the user runs"). No marketing adjectives.

---

## 7. Out of Scope

- Interactive code playgrounds
- Authenticated / gated content
- Localization (revisit if Orbit gains non-English contributors at scale)
- Comments, discussions, or embedded social
- A blog
- Analytics, unless there is a decision to measure something specific
- Versioned docs, until release-specific documentation is needed
- Crate boundaries and dependency direction: contributor material that lives in the repository's `ARCHITECTURE.md`
