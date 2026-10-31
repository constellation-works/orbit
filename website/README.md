# Orbit Website

Documentation site for `orbit-cli.com`. Astro + Starlight.

The site's purpose, visual system, navigation, homepage rules and content conventions are in
[DESIGN.md](DESIGN.md). Read it before changing how the site looks or is organized.

```bash
npm install
npm run dev      # local dev server
npm run check    # astro check (type-checks content and components)
npm run build    # static build into dist/
npm run preview  # serve the built site
```

## Content

Pages are authored by hand under `src/content/docs/`. Two routes under `src/pages/` are
the exceptions: `/changelog/` renders the repository's `CHANGELOG.md`, and `/tasks/` is the
landing for the task links Orbit mints into pull requests. Do not author pages in the
reserved `src/content/docs/metrics/` directory.

Commands and flags shown in these docs must match the CLI that ships. When you change CLI
behavior, check the affected page against `orbit <command> --help` from a current build and
update it in the same pull request.

Internal design records under `docs/design/` and runbooks under `docs/runbooks/` are **not**
published on this site. Link to them on GitHub when a contributor genuinely needs one.

Mermaid fences render to inline SVG at build time through `plugins/mermaid.mjs`
and `beautiful-mermaid`; no browser, client renderer or external service is needed
to build or view them. The optional fence metadata names the accessible diagram.
Declare each node's label on its first mention. Keep graph data in the fence:
the plugin preserves it in a collapsible source alternative. `custom.css`
supplies local fonts and theme colors, with keyboard
and touch scrolling for diagrams wider than the content column.

## Generated assets

`public/og-image.*`, the logo SVGs in `src/assets/` and `public/favicon.svg` are generated
outside this repository. Do not edit them by hand. The dashboard screenshots in
`src/assets/dashboard/` are captured by [`scripts/dashboard-shots/`](scripts/dashboard-shots/README.md).

## Validation and publication

Follow the [website validation runbook](../docs/runbooks/website-validation.md) before
handing off a website change. It covers the local checks, the rendered-page evidence, the
`security.txt` and response-header policy, and manual publication.

After building and starting a local preview, check the crate dependency graph
with the host browser kit (writes screenshots and a JSON report):

```bash
. "$ORBIT_BROWSER_KIT_ENV"
node website/scripts/check-crate-diagram.mjs "$PLAYWRIGHT_MODULE" http://127.0.0.1:4321 .orbit/tmp/crate-diagram
```

Run this command from the repository root. It checks both themes at 1440px and
375px, the complete source alternative, keyboard scrolling, page containment,
and rendering without JavaScript or third-party requests.
