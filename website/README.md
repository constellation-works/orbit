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

The repository-supported publication path is a direct upload to the existing
Cloudflare Pages project that serves the externally managed `orbit-cli.com`
custom domain. The `Website` GitHub Actions workflow builds pull requests, but
publishes only from `main`, Orbit's release/production branch. A push to `main`
that changes `website/**` (or the workflow itself) publishes automatically. A
maintainer can also dispatch the workflow against `main` to recover or repeat
publication.

Publication requires the existing `production` GitHub environment to provide
`CLOUDFLARE_PAGES_PROJECT`, `CLOUDFLARE_ACCOUNT_ID`, and a project-scoped
`CLOUDFLARE_API_TOKEN` with Pages edit access. An account owner must set the
project variable only after confirming that project already owns the
`orbit-cli.com` custom domain. The job has only `contents: read` and
`deployments: write`; it does not create a Pages project or change DNS. It
uploads the exact artifact built earlier in the run, attributes the upload to
the source commit, and verifies the deployment URL plus
`https://orbit-cli.com` before succeeding. The published `/deployment.json`
records the source revision and Actions run URL.

The published `/.well-known/security.txt` is the canonical security-reporting
document. Its `Contact` points to GitHub's private vulnerability-reporting form,
matching [SECURITY.md](../SECURITY.md); its `Policy` points to that policy. The
Orbit maintainers own renewal: review the file before the `Expires` timestamp
and renew it annually when the reporting channel or policy changes.
`npm run validate:security-txt` checks the source file, and the workflow checks
both the source and built asset. After publication, the workflow also requires
the deployment URL and `orbit-cli.com` to return this asset as UTF-8 `text/plain`
and validates the response body.

## Transport security

`public/_headers` is the sole repository-owned response-header policy. Cloudflare
Pages copies it to the static-output root and applies its `Strict-Transport-Security:
max-age=31536000` rule to every HTTPS route, including static error responses.
The bounded one-year policy deliberately omits `includeSubDomains` and `preload`:
the repository does not establish HTTPS readiness or operational ownership for
every subdomain.

HTTP-to-HTTPS redirection is owned by the externally managed Cloudflare zone,
not by the Pages artifact. The production publish job verifies both that redirect
and HSTS on representative success and 404 responses after each deployment.
Changing either responsibility requires updating the workflow and the [website
validation runbook](../docs/runbooks/website-validation.md) in the same change.

Build success is not publication success. The `Check and build` job proves only
that Astro produced static output; the separate `Publish to Cloudflare Pages`
job and its GitHub Deployment record prove upload and post-deploy verification.
See the [website validation runbook](../docs/runbooks/website-validation.md) for
evidence, recovery, and external-blocker handling (ORB-11379).

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
