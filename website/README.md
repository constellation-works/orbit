# Orbit Website

Documentation site for `orbit-cli.com`. Astro + Starlight.

```bash
npm install
npm run dev      # local dev server
npm run check    # astro check (type-checks content and components)
npm run build    # static build into dist/
npm run preview  # serve the built site
```

Every published page is authored by hand under `src/content/docs/`. Nothing on
this site is generated at build time, so `npm run build` is a pure function of
the tracked sources. Before website commands run, a cleanup hook removes the
retired generated `src/content/docs/metrics/` directory left by older checkouts.
Do not author pages in that reserved directory. This hook does not generate
content.

The repository's internal design records under `docs/design/` and its
operational runbooks under `docs/runbooks/` are **not** published here. They
describe implementation history and repository-internal artifacts; the public
site documents current, operator-visible behavior. Link to a runbook on GitHub
when a contributor genuinely needs one.

Commands and flags shown in these docs are expected to match the CLI that
ships. When you change CLI behavior, verify the affected page against
`orbit <command> --help` from a current build and update it in the same pull
request.
