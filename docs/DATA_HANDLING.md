---
type: context
summary: "Orbit Data Handling — what stays on your machine, what leaves it, and to whom"
last_validated: 2026-09-20
---

# Orbit Data Handling

Orbit is local software. It runs on your machine, stores its state on your
disk, and is operated by you. There is no Orbit-hosted service, no account, no
telemetry, and no channel through which the Orbit project receives your code,
tasks, prompts, or logs.

This page is written for the person who has to answer a security or procurement
questionnaire about Orbit. It states what Orbit stores, what it sends, and where
the compliance boundary actually sits.

## The one-paragraph answer

Orbit does not process your data on your behalf. All Orbit state lives under
`~/.orbit/` and `<repo>/.orbit/` on the host that runs it. `<repo>/.orbit/` is
per-user checkout state (gitignored in full); it is not a repository artifact.
The only network
traffic Orbit initiates itself is to fetch its own releases from GitHub when you
run `orbit update` or `orbit semantic install`. Everything else that leaves
the machine — model traffic from agent CLIs, `git push`, pull requests, SSH —
goes through tools and accounts you already control, under those providers'
terms, not Orbit's. Because Orbit is neither a data processor (GDPR), a
business associate (HIPAA), nor a service operator (SOC 2), those frameworks
apply to *your* environment, and Orbit is one locally installed tool inside it —
in the same category as `git` or an editor.

## What Orbit stores, and where

The full inventory, with backup and restore guidance, is in
[state-and-backup](./runbooks/state-and-backup.md). The parts that matter for a
data-handling review:

| Data | Location | Notes |
| --- | --- | --- |
| Task bundles (titles, descriptions, plans, review threads, status) | `~/.orbit/tasks/workspaces/<ws-id>/<task-id>/` | Authoritative. Plain files on disk; may contain whatever you or an agent wrote into a task. |
| Audit events, job runs, step checkpoints, routine state | `~/.orbit/orbit.db` (SQLite) | Authoritative history of what each agent invocation did. |
| Redacted agent output blobs | `<repo>/.orbit/state/audit/blobs/` | Content-addressed; secrets are redacted at write time (see below). |
| Process logs | `~/.orbit/state/logs/orbit.jsonl` | JSONL, rotated locally; secret-looking values are redacted before reaching the sink. See [logging](./runbooks/logging.md). |
| Semantic task index | `<repo>/.orbit/state/semantic.db` | Local vector index; regenerable. Embeddings are computed on the host by the search companion. |
| Worktrees | `<repo>/.orbit/state/worktrees/` | Scratch; regenerable. |
| Machine identity (`machine.id`, `machine.name`, `machine.task_prefix`) | `~/.orbit/config.toml` `[machine]` | A locally generated stable identifier. It is never transmitted to the Orbit project. |
| Workspace registry, runtime config, resource overrides | `~/.orbit/config.toml`, `workspaces.json`, `resources/` | Host-global configuration only. |
| Workspace config, routines, auto-tasks, resources | `<repo>/.orbit/config.toml`, `routines/`, `auto_tasks/`, `resources/` | Per-user checkout settings. Seeded by `orbit workspace init`; not committed. |

Nothing in this table is synchronised anywhere by default. If you want task
history to leave the machine, you opt in explicitly through
[task publication](./runbooks/task-publication.md) to a Git repository you
choose, or through federated MCP over SSH to hosts you configure.

## What leaves the machine

### Initiated by Orbit itself

| Traffic | When | Destination | What is sent |
| --- | --- | --- | --- |
| Release check and binary download | Only when you run `orbit update` | `api.github.com` / `github.com/constellation-works/orbit/releases` | A GitHub API request for the latest release; no identifiers, no payload. |
| Search companion and embedding model download | Only when you run `orbit semantic install` | `github.com/constellation-works/orbit/releases` | Download only. |
| Direct HTTP model transports (Anthropic Messages, OpenAI-compatible, Gemini) | Never from the `orbit` CLI | `api.anthropic.com`, `api.openai.com`, `generativelanguage.googleapis.com` by default | These transports live in the `orbit-agent` library crate for embedders and examples. Every `orbit` crew dispatches through a provider CLI; selecting an HTTP-only provider such as `openai_compat` fails structurally rather than making a request. |

Orbit has no update check on startup, no crash reporter, no usage analytics,
and no "phone home" of any kind. There is no network call you cannot trace to a
command you ran or a crew you configured.

### Initiated by tools Orbit runs for you

Most Orbit work is done by agent CLIs — Claude Code, Codex, Gemini, Copilot,
Cursor, Grok, Pi — running as subprocesses. Those processes talk to their own
providers with their own credentials under their own terms of service. Orbit
does not proxy, log, or inspect that traffic beyond the stdout/stderr it
captures into the audit trail. The same applies to `git`, `gh`, and `ssh`
invoked by an activity: a pull request is opened against the remote you
configured, using the GitHub identity you already have.

If your organisation's compliance posture depends on where model traffic goes,
that decision is made when you choose which agent CLIs and providers to install
and which crews to enable — not by Orbit.

Two specifics worth knowing:

- Agent subprocesses keep host network access. Orbit's OS sandbox is a
  filesystem boundary, not a network boundary. The control on exfiltration is
  the environment allowlist described next.
- Orbit invokes Pi with `--offline`, which suppresses Pi's own version check,
  package update checks, and install telemetry ping. Model API traffic is
  unaffected.

### Optional network surfaces you turn on

| Surface | Default | Notes |
| --- | --- | --- |
| Dashboard (`orbit web serve`) | Binds `127.0.0.1` | Reach a remote workspace with `orbit web connect`, which tunnels over SSH. |
| MCP server (`orbit mcp serve`) | Local stdio, or a loopback-bound TCP listener | A wider bind must be requested explicitly. Remote access is a byte-transparent SSH stdio proxy; authorization is by the SSH identity you configure. |
| Federated MCP | Off | Presents one MCP namespace over this host plus operator-configured SSH remotes. Only hosts you name; there is no registry or discovery. |
| Task publication | Off | Pushes task bundles to a Git repository you bind. |

## Credentials and secrets

- **Provider keys are yours and stay on your host.** Provider CLIs read
  their own credentials — from their credential store (for example the macOS
  login keychain for Claude and Copilot) or from variables you forward through
  `[execution.env].pass`. Orbit never stores a provider key in its own state.
- **Agent subprocesses get an allowlist-composed environment.** Nothing is
  forwarded to an agent unless it is a named Orbit envelope variable or a
  name you list in `[execution.env].pass`. A benignly named credential such
  as `DATABASE_URL` does not reach an agent by accident. See
  [CONFIG.md — `[execution.env]`](./CONFIG.md#executionenv--the-agent-subprocess-environment).
- **Redaction runs before persistence.** Environment values whose names match
  `TOKEN`, `SECRET`, `PASSWORD`, or `API_KEY`; `Authorization` and `x-api-key`
  headers; and `sk-…`-shaped keys are redacted before they reach the log sink
  or an audit blob. Redaction is a safety net, not the boundary — the
  allowlist is the boundary.
- **Keychain access under the sandbox is deny-by-default** with a narrow,
  documented carve-out for the user login keychain item each provider CLI
  needs. System keychains stay denied.

## What the Orbit project receives from you

Nothing, unless you send it. The project has no telemetry endpoint. The only
ways information reaches the maintainers are a GitHub issue, discussion, pull
request, or a private vulnerability report under [SECURITY.md](../SECURITY.md).
Redact anything sensitive before attaching logs or run bundles to an issue.

## Where the compliance boundary sits

| Framework | Does it apply to Orbit? | What it applies to instead |
| --- | --- | --- |
| **GDPR** | No. Orbit is neither a controller nor a processor of your users' data; the project never receives it. | Your organisation, for the personal data in the repositories and tasks you point Orbit at, and the providers your agent CLIs send prompts to. Their DPAs cover that traffic. |
| **HIPAA** | No. Orbit is not a business associate; no PHI is transmitted to or held by the project. | Your environment. If PHI can appear in a repository or a task, ensure the agent providers you enable will sign a BAA, and treat `~/.orbit/` and `<repo>/.orbit/` as PHI-bearing storage on that host. |
| **SOC 2** | Not applicable. There is no Orbit-operated service to audit. | Your own SOC 2 scope, in which Orbit is a locally installed developer tool subject to your endpoint, access, and change-management controls. |

If a questionnaire insists on a SOC 2 report or a signed DPA from "the vendor",
the accurate answer is that there is no vendor-operated service and no vendor
data flow; point the reviewer at this page and at
[SECURITY.md](../SECURITY.md).

## Practical guidance for regulated environments

- **Keep task content out of scope where you can.** Task titles and
  descriptions are stored in plain files and reach every agent that works the
  task. Do not paste customer records or PHI into a task.
- **Choose providers first.** Enable only crews whose provider terms and
  agreements match your obligations; a disabled crew sends nothing.
- **Pin what agents can see.** Use `[execution.env].pass` deliberately and
  the sandbox `fsProfile` / `denyRead` policies to keep secrets and
  out-of-scope directories away from agents.
- **Treat `~/.orbit/` as sensitive storage.** Back it up and encrypt it under
  the same policy as the repositories it references. The
  [state-and-backup](./runbooks/state-and-backup.md) runbook covers WAL-safe
  backups and what is regenerable.
- **Retention is yours.** Logs rotate locally ([logging](./runbooks/logging.md));
  audit history in `orbit.db` is kept until you delete it. Nothing ages out to
  a remote.
- **Air-gapped use works** apart from the two explicit download commands and
  whatever your agent CLIs need. Install the binary and the search companion
  by hand and Orbit initiates no further connections.

## Related

- [SECURITY.md](../SECURITY.md) — reporting vulnerabilities; what is in scope.
- [CONFIG.md](./CONFIG.md) — provider credentials, the subprocess environment
  allowlist, sandbox profiles.
- [state-and-backup](./runbooks/state-and-backup.md) — the complete state
  inventory.
- [logging](./runbooks/logging.md) — log locations, rotation, and redaction.
- [auditability design](./design/auditability/1_overview.md) — what the audit
  trail records and why.
