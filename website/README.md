# Orbit Website

Documentation site for `orbit-cli.com`. Astro + Starlight.

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

## Generated assets

`public/og-image.*`, the logo SVGs in `src/assets/` and `public/favicon.svg` are generated
outside this repository. Do not edit them by hand. The dashboard screenshots in
`src/assets/dashboard/` are captured by [`scripts/dashboard-shots/`](scripts/dashboard-shots/README.md).

## Validation and publication

Follow the [website validation runbook](../docs/runbooks/website-validation.md) before
handing off a website change. It covers the local checks, the rendered-page evidence, the
`security.txt` and response-header policy, and manual publication.
