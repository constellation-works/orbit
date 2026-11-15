# Orbit Website — Design

The design contract for `orbit-cli.com`: who the site is for, how it is organized, how it
looks and how its pages are written. Read it before changing the site's navigation, look or
homepage, and update it in the same pull request.

## 1. Purpose and audience

The site is documentation, not marketing. It serves one reader in two moods:

- **Evaluating.** An engineer who already uses a coding-agent CLI and wants to know what
  Orbit does and what it costs to try. They read the homepage and the Quickstart.
- **Operating.** The same engineer, later, with one job in mind: drain a backlog, fix a
  blocked run, look up a config key. They arrive from search or the sidebar and leave when
  the job is done.

Every page is written for that reader doing one job. A page that restates another page,
documents internals a user never touches, or pads a thin point is merged or removed.

Non-goals: lead generation, a blog, release announcements (the changelog is the record),
live demos, analytics, versioned docs, localization, comments, gated content, and
contributor internals (crate layout, CI policy), which live in the repository.

## 2. Information architecture

The sidebar follows the reader from setup to lookup. Labels are short and name the job.

| Group | Pages | Job |
|---|---|---|
| **Get started** | Quickstart, Install, Connect your agent, Ship a first task, Delivery workflows | From nothing to a shipped task. |
| **Concepts** | Overview, Tasks, Agents and crews, Activities and jobs, Routines and auto-tasks | The model to hold in your head. |
| **Guides** | Overview, then six sub-groups: Deliver, Interfaces, Improve, Multiple machines, Maintain, Extend | One job per page. |
| **Reference** | CLI, Configuration, the YAML formats, Policies, Where state lives, Application consent, Resident mandates | Lookup. |
| **Project** | Changelog, Contributing, Privacy | About the project. |

Rules:

- The header carries the logo (home), the latest-release badge (changelog), the four
  section links and Changelog, search, GitHub and the theme toggle. The homepage is not in
  the sidebar.
- Guides sub-groups other than Deliver start collapsed, so the sidebar fits one screen.
- Section index pages (`/concepts/`, `/how-to/`, `/reference/`) exist because the header
  links to them. They list their pages once, with one line each, and carry no other prose
  that a child page owns.
- A page's URL never changes when it moves between groups. A removed or merged page gets a
  line in `public/_redirects`.
- No "coming soon" pages. `src/content/docs/metrics/` is reserved; do not author there.

### URLs other things depend on

These are linked from the repository `README.md`, the npm page, CLI output or pull requests.
Keep the page and the heading text, or add a redirect (anchors cannot be redirected):

- `/getting-started/install/#install-as-an-agent-plugin`, `/getting-started/first-task/#from-the-terminal`,
  `/getting-started/workflows/`
- `/how-to/mcp-integration/#connect`, `/how-to/mcp-integration/#register-the-federated-mux`,
  `/how-to/experiments/#read-the-readout`
- `/concepts/#plan-and-govern`, `/concepts/#execute-safely-in-parallel`,
  `/concepts/#run-unattended`, `/concepts/#observe-and-extend`
- `/reference/config/#workspace-presets`, `/reference/scoping/#where-state-lives`
- `/changelog/`, and `/tasks/<id>`, which `_redirects` lands on `/tasks/`

## 3. Visual system

Dark is the default theme and light is available; neither is an afterthought. One accent
colour, one type family per role, no decoration. All values are tokens in
`src/styles/tokens.css`; no other stylesheet declares a colour, a type size or a spacing
step of its own.

### 3.1 Palette

| Role | Dark | Light | Notes |
|---|---|---|---|
| Background | `#0A0A0A` | `#FFFFFF` | Reading surface. |
| Surface | `#17171A` | `#F4F5F8` | Code, tables, command surfaces, row hover. |
| Line | `#26262B` | `#D8DBE2` | Structural hairlines only. |
| Text | `#EDEDF0` | `#17171A` | Headings and emphasised text. |
| Text, secondary | `#C7C7CC` | `#30313A` | Prose. |
| Text, muted | `#9B9BA3` | `#5F606B` | Metadata, captions, eyebrows. |
| Accent | `#6E9FFF` | `#2F67D8` | Links, the active page, focus, the primary button. |

A colour means one thing: the accent marks what you can follow or what is current, and
amber appears only on the changelog's breaking-changes panel.

`:root` carries the dark tokens, so a page renders dark without JavaScript;
`:root[data-theme='light']` overrides them. The inline script in `ThemeProvider.astro`
sets `data-theme` before first paint: light only when the visitor's stored choice
(`orbit-theme-choice`) is `light`, dark otherwise, whatever the OS prefers. A plain page
load saves nothing; the toggle saves an explicit choice.

### 3.2 Type

Geist for text and Geist Mono for code, both self-hosted from `@fontsource-variable`, so the
site loads no fonts from other domains. One scale:

| Token | Size | Used for |
|---|---|---|
| `--orbit-text-xs` | 12px | Eyebrows (sidebar group labels, "On this page"), badges, state pills. |
| `--orbit-text-sm` | 13px | Captions and metadata. |
| `--orbit-text-ui` | 14px | Navigation, code, tables, secondary copy. |
| `--orbit-text-body` | 16px | Prose (line-height 1.65). |
| `--orbit-text-lede` | 18px | `h3` and the homepage lede. |
| `--orbit-text-h2` | 22px | `h2`. |
| `--orbit-text-h1` | 32px | Page titles. |
| `--orbit-text-display` | 44px | The homepage headline, and nothing else. |

Elements of the same kind share a size and weight wherever they appear.

### 3.3 Space and shape

One spacing scale, `--orbit-space-1` to `-8` (4, 8, 12, 16, 24, 32, 48, 72px). Sibling
elements share padding and gaps. Radius is 8px for surfaces and 4px for inline code.
Nothing has a shadow. A surface does not sit inside another surface.

### 3.4 Layout

Documentation pages use Starlight's three columns: sidebar, a content column of about
46rem, and an "On this page" rail.

- The sidebar's group labels are small uppercase eyebrows; the links under them are what a
  reader scans. The active page has a 2px accent bar on its left edge.
- Each `h2` opens with a hairline and a fixed step of space above it, so a long page reads
  as a list of sections. The first heading on a page has neither.
- Tables keep their rows readable at 375px: the configuration and CLI tables have their own
  wrapping rules in `prose.css` and the markup hooks in `astro.config.mjs`
  (`documentation-tables`) and `plugins/inline-code.mjs`.
- No page scrolls sideways at 375px or 1440px. A wide table or code block scrolls inside
  its own box.
- Below 50rem the homepage shows a Menu disclosure (`HomeMenu.astro`) for the section links
  and theme; documentation pages keep Starlight's sidebar menu.
- The footer is one line: the licence and telemetry statement, and links to GitHub,
  Changelog, Contributing and Privacy.

### 3.5 Components

Each exists once, in `src/styles/components.css` or `home.css`, and looks the same wherever
it is used.

| Component | Class | Where |
|---|---|---|
| Index list: a title and one line, the row a link | `.orbit-index` | Section indexes, the homepage, `/tasks/`. |
| Layer stack | `.orbit-stack` | Concepts overview. |
| Step chain | `.orbit-pipeline` | Routines and auto-tasks. |
| Command with a copy button | `.orbit-command`, `.orbit-commands` | Homepage. |
| Release disclosure and breaking-changes panel | `.orbit-release` | Changelog. |

Cards are not a component. A list of links is an index list; a list of facts is a table or
a plain list.

### 3.6 Homepage

The homepage answers, in order: what is this, how does it work, what can I rely on, where
does a run stop, how do I start. It runs on the splash template, 72rem wide, and replaces
Starlight's title with its own `h1`.

1. **Hero.** A headline that says what Orbit is, a lede that walks one change from request
   to pull request, the install command with a copy button, two buttons (Quickstart,
   Connect your agent), the platform line and the agent CLIs Orbit drives. Beside it, the
   dashboard tour.
2. **How it works.** An example conversation beside the four steps it maps to, each tagged
   with the task status it leaves.
3. **What you can rely on.** Four statements on one grid.
4. **Choose where a run stops.** A table: the command, where it stops, what `--complete`
   adds.
5. **Ship your first task.** The four commands, each with a copy button.
6. **Go further.** An index list of the next pages.

Rules for the page:

- No tagline, no feature the CLI does not ship, no live metric. Commands match the CLI;
  an example says it is an example.
- The video is the site's only motion: muted, looping, paused on its poster under
  `prefers-reduced-motion: reduce`, with an `aria-label` that narrates it and a caption
  that dates the capture. Its source lives in `marketing/media/video/orbit-dashboard-tour-v4`;
  the site serves a web re-encode from `public/media/`. Public cuts omit production task,
  friction and run identifiers. Sanitize before encoding and inspect decoded frames before
  publishing.
- Only shipped, current agent CLIs are named. A legacy executor is not listed.
- Scripts are limited to the theme script, the Menu's Escape and breakpoint close, the copy
  buttons, and pausing the video under reduced motion.

## 4. Writing

1. **Lead with what the reader can do.** A page opens with one or two sentences that say
   what it lets you do. No "this page describes", no history.
2. **Shortest path first.** The command or the click, then what to expect. Edge cases and
   recovery come after, under headings that name the situation.
3. **Plain words, short sentences**, second person, present tense. A table or list beats a
   paragraph that enumerates. An example beats a description of one.
4. **Every paragraph changes what the reader does or decides.** Cut implementation
   internals, exhaustive refusal catalogues, compatibility archaeology, and anything
   another page owns: link to the owner instead.
5. **Accurate or absent.** Every command, flag and key shown matches the CLI that ships;
   check against `orbit <command> --help` from a current build. Removing a claim is fine; a
   wrong one is not.
6. No taglines, filler or marketing adjectives. No internal task or friction identifiers
   (the changelog, which renders `CHANGELOG.md` as is, is the one exception), no
   source-tree paths, no links to repository design docs.

Mechanics:

- Frontmatter needs `title` and `description`; the description is one plain sentence.
- Content headings start at `h2`. A heading's text is its anchor, so renaming a heading
  breaks links to it: check §2's list and search the site before you rename.
- Links are site-relative with a trailing slash (`/how-to/dashboard/#tasks`).
- Code fences always name a language. Asides (`:::note`, `:::tip`, `:::caution`) are for
  something the reader would be hurt by missing: at most one or two a page.
- Pages are Markdown, written by hand. Two routes under `src/pages/` are the exceptions:
  `/changelog/` renders the repository's `CHANGELOG.md`, and `/tasks/` is the landing for
  the task links Orbit mints into pull requests.

## 5. Stack

- **Framework:** Astro with Starlight. Content is rendered at build time.
- **Search:** Pagefind, built into Starlight: static, no third-party account.
- **Styles:** `src/styles/tokens.css` (tokens), `prose.css` (page chrome and prose),
  `components.css` (shared components) and `home.css` (the homepage), loaded in that order
  through `customCss` in `astro.config.mjs`.
- **Components:** overrides of Starlight's `Header`, `SiteTitle`, `ThemeProvider`,
  `ThemeSelect`, `Footer` and `PageTitle` in `src/components/`.
- **Hosting:** Cloudflare serves the static output; Daniel publishes it by hand. DNS is
  managed outside this repository. Cloudflare Pages applies `public/_headers` and
  `public/_redirects`.
