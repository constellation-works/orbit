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
| **Agent CLIs** (Claude Code, Codex, …) | `sandbox-exec`. Writes are confined. Reads are allowed everywhere except a credential denylist and Orbit's own secret stores. Network is open. | Bubblewrap (`/usr/bin/bwrap`, or Orbit's signed bundled build at the root-owned `/usr/local/libexec/orbit/bwrap` when the host's is missing or lacks `--bind-fd`). Writes are confined by the `modify` policy. Host reads and network stay open, except masked credential locations, Orbit's own secret stores and per-user runtime sockets (see Linux IPC below). Fails closed if bwrap is missing, unless the executor sets `allow_fallback: true`. |
| **`proc.spawn` in activities** | The child inherits the enclosing worker's `sandbox-exec` boundary; there is no separate macOS refusal. | The child inherits the enclosing worker's Bubblewrap boundary; no separate Landlock ruleset is applied. |

**Linux IPC.** Every Bubblewrap plan hides the existing `/run/user/<uid>` directory and any existing absolute `XDG_RUNTIME_DIR` behind empty mounts, after policy and caller mounts. This blocks pathname connections to the host's session D-Bus, systemd user manager, SSH agent and GPG agent in those directories, including from inherited `proc.spawn` children. A grant inside a masked directory, or a bind alias exposing it elsewhere, refuses the launch. No raw session bus is admitted for provider authentication. This is not general IPC isolation: the shared network namespace leaves abstract Unix sockets and network-reachable services accessible, and pathname sockets outside these runtime directories (such as the system bus under `/run/dbus`) are not masked. A directory absent at launch receives no mask. Host services must enforce their own authorization; a read-only filesystem bind alone does not prevent socket connections.

**Git metadata (macOS and Linux).** Sandboxed agent launches deny writes to the registered checkout and active checkout's `.git` entry, per-worktree Git directory, and shared Git common directory, including config, attributes, hooks, refs, and recovery payloads. These denies follow provider and runtime write grants, so a worktree grant cannot reopen them. Writable directories above these paths, such as the checkout itself, cannot be renamed aside to carry the metadata out of the denies: Linux binds them as mount points, and macOS denies writes to the directory entries themselves. Preparation rejects symlinked, special-file, or hard-linked metadata rather than leave writable aliases; the one exception is Git's own temporary object, pack and index names whose every link is inside the same `objects/` store. Git inspection remains readable. macOS implementer source grants remain anchored at the registered checkout, with the active worktree separately writable.

**Orbit secret stores (macOS and Linux).** Every sandboxed agent launch hides plugin state (`<global_root>/state/plugins/`), the plugin secret store (`<global_root>/state/plugin-secrets/`), and the clock credentials file `<global_root>/clock.env`, for reads and writes. The paths are resolved under the configured global root, which need not be `~/.orbit`. Both platforms also hide the SSH control-socket directory `~/.orbit/ssh/`, resolved from the same home directory as remote and federated SSH sessions, independently of the configured global root. The host creates this directory before launch so the mask can cover it; sandboxed agents cannot connect to its authenticated SSH masters. macOS appends explicit outbound Unix socket denies beneath the masked trees after the broad network allow. Host-side remote and federated MCP sessions retain access to their SSH masters. The clock tick runs outside the sandbox and gives each workspace only the `clock.env` names its `[execution.env] pass` admits, so an agent can't read the file to collect the others. These denies are appended after every other rule, so no policy grant reopens them. macOS denies `clock.env` whether or not it exists. Linux binds `/dev/null` over it, and Bubblewrap can't mount over a missing path, so a `clock.env` created after an agent starts is readable to that agent.

**Glob denies (macOS).** Seatbelt matches pathnames, so `denyModify` and `denyRead` globs such as `**/.env` are regexes rooted at the workspace, and a rename is checked against the moved entry only. When the profile is compiled, Orbit finds every existing match and denies writes to its writable ancestor directories, so a directory holding a `.env` can't be moved to `/tmp` to change or read the file there and moved back. A match created after the sandbox starts gets no such pin, so its directory can still leave the workspace. A path denied for reading but not for modifying can be renamed out of its read deny; the default policy denies both for `.env` names.

**Credential denylist (macOS and Linux).** The shared denylist covers `~/.ssh`, `~/.aws`, `~/.config/gh`, the user and system Keychains, Cargo's publish token, and browser profile stores. macOS denies `~/Library/Application Support/{Google/Chrome,Chromium,BraveSoftware/Brave-Browser,Firefox}`. Linux masks the default trees `~/.config/google-chrome`, `~/.config/chromium`, `~/.config/BraveSoftware/Brave-Browser` and `~/.mozilla/firefox`, and only when each path exists at launch. Chrome, Chromium and Brave honor `XDG_CONFIG_HOME`: a profile under any directory other than `~/.config` is not masked. Firefox does not use `XDG_CONFIG_HOME`; a profile outside `~/.mozilla/firefox` is not masked, including snap (`~/snap/firefox/common/.mozilla/firefox`) and flatpak (`~/.var/app/org.mozilla.firefox/.mozilla/firefox`). Snap copies of Chrome or Chromium, channel directories (`google-chrome-beta`, `google-chrome-unstable`), other browsers, and an explicit user-data directory are not covered. The headless browser kit at `~/.local/chromium-deps` is outside these roots and stays readable. The denylist is known to be incomplete (for example, `~/.netrc`, `~/.git-credentials`, `~/.gnupg`, `~/.docker/config.json`, `~/.kube/config`, `~/.npmrc`, and cloud-CLI caches aren't on it). With network open, treat this read scoping as advisory, not a security boundary.

**Provider-specific carve-outs (macOS):**
- Every supported provider's state directory (`~/.claude`, `~/.codex`, `~/.gemini`, `~/.grok`) is writable in every run, whichever provider is active. A file planted in another provider's directory could persist across sessions.
- Claude, Copilot, Cursor, Antigravity, and Muse runs may *read* `~/Library/Keychains` for credentials stored there. Nothing gets keychain writes. The system keychains stay denied, and an `fsProfile` that denies `~/Library` or `~/Library/Keychains` still wins.
- Sandboxed Codex gets `CODEX_CA_CERTIFICATE=/etc/ssl/cert.pem`, unless you set that or `SSL_CERT_FILE` yourself. The bundle holds public trust anchors only and doesn't weaken TLS verification.

**Environment.** Agent subprocess environments are built from an allowlist: a documented baseline, your `[execution.env]` pass list, and the variables each provider declares it needs. Nothing is inherited just because it has a harmless-looking name. Orbit adds the headless browser kit's locators (`PLAYWRIGHT_BROWSERS_PATH`, `PLAYWRIGHT_MODULE`, `ORBIT_BROWSER_KIT_ENV`) only while the kit under the child's home has passed `orbit browser-kit verify` at its installed version. They name paths and carry no secret. The kit's unpacked libraries reach only a shell that sources its `env.sh`, never every agent's `LD_LIBRARY_PATH`, so they cannot shadow libraries other tools load.

**Symlinks.** Policy matches the real path. Orbit canonicalizes the target, or the nearest existing ancestor for a new file, and follows dangling links to their target. A symlink from an allowed tree into a denied one is denied, and so is any path that resolves outside the workspace.

**Race conditions.** There is an inherent time-of-check-to-time-of-use gap between a policy decision and the filesystem operation that follows. On Linux, Landlock binds to inodes that exist at spawn, so a denied file created afterward is readable. Treat a workspace an attacker can modify concurrently as untrusted.

Details are in [docs/design/policy-sandbox/](docs/design/policy-sandbox/) and the [Linux sandbox runbook](docs/runbooks/linux-sandbox.md).
