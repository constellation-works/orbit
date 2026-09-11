# Orbit Website

Documentation site for `orbit-cli.com`. Astro + Starlight.

```bash
npm install
npm run dev      # local dev server
npm run check    # astro check (type-checks content and components)
npm run build    # static build into dist/
npm run preview  # serve the built site
```

## Production publication

Daniel deploys `orbit-cli.com` manually. GitHub Actions no longer publishes the
site, and this repository does not manage the Cloudflare project or DNS.
Prepare the static output with `npm run check`, `npm run build`, and the local
validation steps in the [website validation runbook](../docs/runbooks/website-validation.md),
then hand the resulting `dist/` output to Daniel for publication.

The published `/.well-known/security.txt` is the canonical security-reporting
document. Its `Contact` points to GitHub's private vulnerability-reporting form,
matching [SECURITY.md](../SECURITY.md); its `Policy` points to that policy. The
Orbit maintainers own renewal: review the file before the `Expires` timestamp
and renew it annually when the reporting channel or policy changes.
`npm run validate:security-txt` checks the source file; run it against the built
asset as well before handing the output to Daniel. After a manual publication,
Daniel can verify that `orbit-cli.com` serves this asset as UTF-8 `text/plain`
and that its response body passes the validator.

## Transport security

`public/_headers` is the sole repository-owned response-header policy. Cloudflare
Pages copies it to the static-output root and applies its `Strict-Transport-Security:
max-age=31536000` rule to every HTTPS route, including static error responses.
The bounded one-year policy deliberately omits `includeSubDomains` and `preload`:
the repository does not establish HTTPS readiness or operational ownership for
every subdomain.

HTTP-to-HTTPS redirection is owned by the externally managed Cloudflare zone,
not by the Pages artifact. Daniel owns the corresponding post-publication
checks for HSTS and redirects. See the [website validation runbook](../docs/runbooks/website-validation.md)
for local evidence and manual-publication verification.

Every published page is authored by hand under `src/content/docs/`, with two
exceptions under `src/pages/`: `/changelog/` renders the repository's tracked
`CHANGELOG.md` so the site never carries a drifting copy, and `/tasks/` is the
landing for the task links Orbit mints into pull requests (`public/_redirects`
sends `/tasks/<id>` there). Nothing on this site is fetched or generated at
build time, so `npm run build` is a pure function of the tracked sources. Before website commands run, a cleanup hook removes the
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
