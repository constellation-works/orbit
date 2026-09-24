# Security Policy

## Reporting a vulnerability

Report security issues privately through [GitHub private vulnerability reporting](https://github.com/constellation-works/orbit/security/advisories/new). Don't open a public issue, PR, or discussion for them.

Include the affected version or commit, your environment, steps to reproduce, and expected versus observed behavior. A proof of concept or a suggested fix is welcome but not required.

This is a small project, so response is best-effort:

| Stage | Target |
|---|---|
| Acknowledgement | within 7 days |
| Triage (accepted, declined, or needs more info) | within 30 days |
| Fix and disclosure | coordinated with you once a patch exists |

Reporters are credited in the advisory unless they prefer otherwise. Declined reports get a written explanation.

## Supported versions

Fixes land on `main` and the latest tagged release. Older tags don't get backports.

## Scope

**In scope:**
- The `orbit` CLI, runtime, and crates published from this repository
- Bypasses of filesystem-scoping policy (`fsProfile`, `denyRead`, `denyModify`)
- Sandbox or process-supervision escapes in `orbit-exec`
- Audit-log tampering or omission
- Auth, authorization, or origin-check bypasses on `orbit web serve` and `orbit mcp serve`
- Handling and redaction of provider credentials

**Out of scope:**
- Vulnerabilities in upstream dependencies. Report those upstream, and we'll bump once a fix ships.
- Attacks that already need local code execution as the Orbit user, or write access to the workspace, unless they cross a documented trust boundary below
- Social engineering of maintainers
- Forks and third-party redistributions

## Sandbox model and known limits

Agent filesystem access is scoped by an `fsProfile` (`read` and `modify` globs) plus global `denyRead` and `denyModify` rules. `orbit-policy` evaluates those rules. How strongly they're *enforced* depends on the platform and on what is running. The limits below are known and documented; reports that go beyond them are in scope.

| What runs | macOS | Linux |
|---|---|---|
| **Agent CLIs** (Claude Code, Codex, …) | `sandbox-exec`. Writes are confined. Reads are allowed everywhere except a credential denylist. Network is open. | Bubblewrap (`/usr/bin/bwrap`). Writes are confined by the `modify` policy. Host reads and network stay open. Fails closed if bwrap is missing, unless the executor sets `allow_fallback: true`. |
| **`proc.spawn` in activities** | Refused, with a capability error | Landlock ruleset applied between `fork` and `exec`, covering the child and all its descendants. Refused on kernels without Landlock ABI 2. |

**Credential denylist (macOS reads).** The denylist covers `~/.ssh`, `~/.aws`, `~/.config/gh`, the user and system Keychains, browser profile stores, and Cargo's publish token. It is known to be incomplete (for example, `~/.netrc`, `~/.git-credentials`, `~/.gnupg`, `~/.docker/config.json`, `~/.kube/config`, `~/.npmrc`, and cloud-CLI caches aren't on it). With network open, treat macOS read scoping as advisory, not a security boundary.

**Provider-specific carve-outs (macOS):**
- Every supported provider's state directory (`~/.claude`, `~/.codex`, `~/.gemini`, `~/.grok`) is writable in every run, whichever provider is active. A file planted in another provider's directory could persist across sessions.
- Claude runs may *read* `~/Library/Keychains` so they can refresh their OAuth session. Nothing gets keychain writes. The system keychains stay denied, and an `fsProfile` that denies `~/Library` or `~/Library/Keychains` still wins.
- Sandboxed Codex gets `CODEX_CA_CERTIFICATE=/etc/ssl/cert.pem`, unless you set that or `SSL_CERT_FILE` yourself. The bundle holds public trust anchors only and doesn't weaken TLS verification.

**Environment.** Agent subprocess environments are built from an allowlist: a documented baseline, your `[execution.env]` pass list, and the variables each provider declares it needs. Nothing is inherited just because it has a harmless-looking name.

**Symlinks.** Policy matches the real path. Orbit canonicalizes the target, or the nearest existing ancestor for a new file, and follows dangling links to their target. A symlink from an allowed tree into a denied one is denied, and so is any path that resolves outside the workspace.

**Race conditions.** There is an inherent time-of-check-to-time-of-use gap between a policy decision and the filesystem operation that follows. On Linux, Landlock binds to inodes that exist at spawn, so a denied file created afterward is readable. Treat a workspace an attacker can modify concurrently as untrusted.

Details are in [docs/design/policy-sandbox/](docs/design/policy-sandbox/) and the [Linux sandbox runbook](docs/runbooks/linux-sandbox.md).
