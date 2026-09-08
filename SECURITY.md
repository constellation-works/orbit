# Security Policy

## Supported Versions

Orbit is pre-1.0 and ships from `main`. Security fixes land on `main` and the most recent tagged release; older tags do not receive backports.

| Version       | Supported          |
| ------------- | ------------------ |
| `main` (HEAD) | :white_check_mark: |
| Latest tag    | :white_check_mark: |
| Older tags    | :x:                |

## Reporting a Vulnerability

Please report security issues privately via GitHub: open the repository's **Security** tab and choose **Report a vulnerability** ([private vulnerability reporting](https://github.com/constellation-works/orbit/security/advisories/new)).

Do **not** open a public issue, pull request, or discussion for suspected vulnerabilities.

Include enough detail to reproduce: affected version or commit, environment, steps, observed vs. expected behavior, and any proof-of-concept. A suggested fix is welcome but not required.

## What to Expect

This is a small project, so response is best-effort:

- **Acknowledgement:** within 7 days.
- **Triage and assessment:** within 30 days, including whether the report is accepted, declined, or needs more information.
- **Fix and disclosure:** coordinated with the reporter once a patch is available. Reporters are credited in the advisory unless they prefer to remain anonymous.

If a report is declined, you'll get a written explanation of why (out of scope, intended behavior, mitigated elsewhere, etc.).

## Scope

In scope:

- The `orbit` CLI, runtime, and crates published from this repository.
- Filesystem-scoping policy bypasses (`fsProfile`, `denyRead`, `denyModify`).
- Sandbox / process supervision escapes in `orbit-exec`.
- Audit log tampering or omission paths.
- Authentication, authorization, or origin-check bypasses on `orbit web serve` and `orbit mcp serve`.
- Credential handling and redaction for provider keys.

Out of scope:

- Vulnerabilities in upstream dependencies — please report those upstream. We'll bump the dependency once a fix is available.
- Issues that require an attacker who already has local code execution as the user running Orbit, or write access to the workspace, unless they cross a documented trust boundary.
- Social-engineering or phishing of project maintainers.
- Findings against forks or third-party redistributions of Orbit.

## Filesystem Policy Enforcement

Agent filesystem access is scoped by an `fsProfile` (`read` / `modify` globs)
plus global `denyRead` / `denyModify` rules, evaluated by `orbit-policy`.

**Enforcement layers differ by platform, and by backend:**

For built-in Orbit tools that still consult `orbit-policy` (today `proc.*`
program allowlists, not a shipped `fs.*` family), evaluation applies on every
platform.

**Activity-scoped `proc.spawn` is confined at the process boundary on Linux.**
An allowed program decides for itself what to open — `bash`, `sh`, `python3`,
and `git` shell aliases are all on shipped activity allowlists — so inspecting
path-shaped arguments cannot scope reads. Orbit compiles the activity's
resolved `fsProfile` into a Linux Landlock ruleset and applies it to the child
between `fork` and `exec`, so the child and every descendant it spawns are held
to it. Outside the workspace the child reads only a fixed table of runtime,
resolver, and CA-trust paths plus the tool state directories its own
environment names; `/etc`, `/proc`, `$HOME`, `~/.ssh`, and `~/.aws` are never
granted as trees. A kernel without Landlock ABI 2 — or any non-Linux host —
makes activity-scoped `proc.spawn` fail with a capability error rather than
run the child unconfined. Landlock rules bind to inodes that exist at spawn, so
a `denyRead` match created *after* the ruleset is compiled is readable by that
child; existing denied files cannot be read, renamed out of reach, or relocated
into a readable directory. See
`docs/design/policy-sandbox/2_design.md` §7.3.

For `backend: cli` agents (an agent CLI such as Codex/Claude/Gemini/Grok
spawned as a subprocess, making its own syscalls outside Orbit's tool
surface), enforcement differs sharply by platform:

| Platform | What actually constrains a `backend: cli` agent's own syscalls |
| --- | --- |
| **macOS** | The `orbit-exec` seatbelt (`sandbox-exec`) profile is a **write** boundary, not a meaningful **read** boundary. The compiled profile emits a blanket `(allow file-read*)` for the whole filesystem, minus a small fixed credential denylist (`~/.ssh`, `~/.aws`, `~/.config/gh`, browser keychains/profile stores, system Keychains) — the `fsProfile`'s positive `read` globs are never emitted into the seatbelt at all (only `read` *deny* entries are). Combined with the unrestricted `(allow network*)` the profile also grants (agents need to reach provider APIs), a `backend: cli` agent's read scope is effectively "everything except the denylist," with an open network egress path — an exfiltration exposure. The credential denylist is also known-incomplete: it does not cover `~/.netrc`, `~/.git-credentials`, `~/.gnupg`, `~/.docker/config.json`, `~/.kube/config`, `~/.npmrc`, or cloud-CLI credential caches (e.g. `~/.aws/sso/cache`, `~/.config/gcloud`, `~/.azure`). Treat the macOS seatbelt's read scoping as **advisory, not a security boundary**, for `backend: cli` agents. Tracked in `ORB-10233`. |
| **Linux** | Orbit runs `backend: cli` agents through trusted `/usr/bin/bwrap` after a namespace-and-mount capability probe. Bubblewrap read-only binds the host root and applies ordered writable mounts from the resolved `modify` policy, so writes are confined while host filesystem reads remain available. It uses `--share-net`, so host network access remains available and network egress is not policy-gated by this boundary. If `/usr/bin/bwrap` is absent or the probe fails, dispatch fails closed unless the executor explicitly sets `allow_fallback: true`; that escape hatch runs the agent without Linux write confinement. |

Additionally, on macOS the seatbelt profile allows writes to each supported
provider's state directory (`~/.claude`, `~/.codex`, `~/.gemini`, `~/.grok`)
unconditionally, regardless of which provider is actually active for the
run — a config or hook file dropped in another provider's state directory is
a cross-session persistence vector, not scoped to the current agent's own
provider. Tracked in `ORB-10234`.

**The macOS credential denylist has one provider-scoped exception.** Claude
Code stores its OAuth session in the macOS login Keychain (item
`Claude Code-credentials`), not in a file under `~/.claude`, so the blanket
`~/Library/Keychains` deny made every sandboxed Claude run fail with
`OAuth session expired and could not be refreshed` — a login that is present
and valid, merely unreadable. When (and only when) the confined CLI is Claude,
the compiled profile re-allows **reads** of `~/Library/Keychains` after the
deny. Codex, Gemini, Grok, and any unrecognized provider name keep the full
deny; `/Library/Keychains` and `/System/Library/Keychains` stay denied for
every provider; nothing grants keychain *writes*, so a sandboxed run can use a
refreshed token but cannot persist it — re-authentication remains an
unsandboxed operation. Reading the keychain file is not the same as reading its
secrets (items stay encrypted behind their own per-item ACLs), but this does
widen a Claude agent's reach to the login keychain file itself, which is why it
is scoped to the one provider that needs it. The exception is a default, not an
override: the compiled clause order is default credential denies, then the
provider carve-out, then the activity's own negated `read` rules, so an
`fsProfile` that denies `~/Library/Keychains` — or any ancestor of it, such as
`~/Library` — takes the read back from Claude under SBPL last-match-wins. A
failing run says which case it hit: Orbit attaches its own attribution to the
provider's misleading "expired" message, and only recommends re-authenticating
when the credential really was reachable.

**Sandboxed Codex uses a public file-backed CA bundle on macOS.** The system
Keychain read denies above prevent Codex's rustls WebSocket client from
completing native-root discovery even though its HTTPS backend can still
connect. For a Codex child under `macos-sandbox-exec`, Orbit preserves an
explicit non-empty `CODEX_CA_CERTIFICATE` first and `SSL_CERT_FILE` second; when neither
is present, it sets `CODEX_CA_CERTIFICATE=/etc/ssl/cert.pem`. Orbit verifies
that the selected path is a readable file before launching the child and fails
with the variable and path when it is not. This supplies public trust anchors;
it does not disable TLS verification or re-allow `/Library/Keychains`,
`/System/Library/Keychains`, `~/Library/Keychains`, or any other credential
directory. Activity `denyRead` rules remain later SBPL clauses and can still
deny the selected bundle. Other providers, Linux, and bare Codex invocations do
not receive this Orbit-managed default.

**Environment forwarding to sandboxed/subprocess agents is name-based, not
value-shaped.** Orbit filters ambient environment variables passed to
provider subprocesses by matching variable *names* against a fixed list of
substrings (`SECRET`, `TOKEN`, `PASSWORD`, `API_KEY`, `_KEY`, `PRIVATE`,
`CREDENTIAL`, `COOKIE`, `SESSION`, `BEARER`, `AUTH`). A secret held in a
benignly-named variable — `DATABASE_URL`, a bare connection string, an
internal service URL with embedded credentials — is forwarded unredacted to
any spawned agent, including `backend: cli` agents with open network access.
Tracked in `ORB-10235`.

**Remaining follow-up remediation** (filed from the pre-release security review,
`SECURITY-REVIEW-2026-07-15.md`, findings H2/M6/M7/M9): `ORB-10233` (macOS
seatbelt positive read allowlist), `ORB-10234` (scope provider state-dir
writes to the active provider), and `ORB-10235` (env forwarding allowlist
instead of denylist). Linux write confinement is provided by the
`linux-bwrap` backend described above; read and network isolation remain
delegated.

**Symlink resolution.** Policy rules are matched against the *real*
filesystem location, not the requested path. Before matching, the requested
path is resolved with `Path::canonicalize` (following symlinks); for a
not-yet-existing target (a write/create) the nearest existing ancestor is
canonicalized and the remaining components are rejoined. **Dangling** symlinks
are followed too (with an `ELOOP`-style traversal cap): an `O_CREAT` open
through a dangling link creates the link's *target*, so evaluation happens at
that target, not at the link path. A symlink inside an allowed subtree that
points into a denied subtree is therefore **denied**, and a resolved path that
escapes the workspace root is denied outright. This resolution lives in
`orbit-policy` (`PolicyEngine::check_resolved` / `resolve_symlinks`) and is
shared by the tools-layer workspace-boundary check, so the guarantee holds
regardless of the caller.

**Known limitation (TOCTOU).** There is an inherent time-of-check to
time-of-use gap between the policy decision and the actual filesystem
operation: an attacker who can race the filesystem (swap a resolved path for a
symlink between check and use) is an OS-level concern outside what the policy
layer can close. On macOS the seatbelt provides a second enforcement point; on
Linux, treat a workspace an attacker can concurrently mutate as untrusted.
