# Orbit Website — Design

**Status:** Draft
**Owner:** Orbit contributors
**Last updated:** 2026-09-27

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
- **Landing page:** the hero carries one demo video of an illustrative session (see 3.5), not an orbit diagram. It is the only motion on the site and stops for readers who ask for reduced motion.
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

The homepage uses an in-content hero in place of Starlight's auto-rendered title (which is hidden via a scoped CSS rule on the homepage only).

The hero stacks three rows:

**Copy** — opens like the sections below it. On the left, the release chip
(`Early access`, linking to the changelog) over a 3.6rem headline whose last
sentence is muted. On the right, a lede that says what Orbit is, primary and
secondary CTAs, and a one-line requirements note, bottom-aligned with the
headline. Below 54rem the two columns stack.

**Demo video** — full content width, because the 16:9 frame carries its own
text and is unreadable in a half column. A silent, looping 31 s render of an
illustrative session from `marketing/media/video/orbit-readme-demo`: the
reader's agent CLI on the left and the task Orbit holds on the right, through
file → approve → ship → review → scale → record. The site serves a web
re-encode at `public/media/orbit-demo.mp4` with a poster frame from the review
beat. It autoplays muted with native controls; under
`prefers-reduced-motion: reduce` it stays on the poster. The `figcaption` says
it is illustrative, and the `aria-label` narrates the session.

**Provider strip** — the shipped CLI executors as a plain list under a
hairline, with the legacy Gemini executor named in a footnote rather than
implied current.

Below the hero, in order:

1. **Guarantees** — three short promises (nothing runs until you approve,
   nothing merges without you, every step is on the record), each with a
   glyph.
2. **How it works** — the lifecycle rail (`proposed → backlog → in-progress →
   review → done`, the default ship's stop at `review` marked and `done`
   dashed) and a 4-card walkthrough (ask → file → ship → review), each card
   with the command or MCP call behind it and the review card outlined in the
   accent.
3. **Why Orbit** — a 2×2 value-prop grid with glyphs and a command per card.
4. **When you step away** — one card per unattended shape (`orbit run ship`,
   `--mode local`, `orbit run auto`, `orbit run ship-sweep`): the command,
   where it stops, and what `--complete` does. Each links to its guide.
5. **Go further** — a list of five guides beside the section head, with the
   CLI reference linked from the head.
6. **Quickstart** — a closing panel with the three setup commands, then
   `orbit web serve` to open the dashboard, and CTAs.

Sections open on a two-column head — mono eyebrow and a one-sentence heading
on the left, a short lede on the right. The footer, not the page, carries the
full docs index.

Commands shown on this page must match current CLI behaviour, and illustrative
output must say that it is illustrative. The page advertises no unlanded feature
and publishes no live metric.

The homepage's extra script handles the Menu's Escape / breakpoint close. The
shared theme script applies the dark default (or a stored light choice) before styles load. The Quickstart
section below the hero retains the install and setup commands.

Other pages keep Starlight's default chrome (auto title, sidebar, TOC) unchanged.

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
- **Internal references:** do not publish repository-internal artifact identifiers in docs pages. The one exception is the changelog page: it renders `CHANGELOG.md` as-is, and its bracketed task IDs stay because they are the pointer from a release note to the pull request that delivered it.
- **Voice:** terse, declarative, second-person ("you run", not "the user runs"). No marketing adjectives.

---

## 7. Open Questions

1. **Versioning.** Starlight supports versioned docs via directory structure. Add it when release-specific documentation becomes necessary.
2. **Architecture detail.** Crate boundaries and dependency direction are contributor material, not published here; they live in the repository's `ARCHITECTURE.md`. Revisit only if a public extension surface makes them user-facing.
3. **Logo refinement.** The ring-with-offset-dot logo ships as `src/assets/orbit-logo-light.svg` and `orbit-logo-dark.svg`. Revisit only if it fails legibility at 16px favicon size.
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
