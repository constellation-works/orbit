---
type: context
summary: "Orbit Data Handling — what stays on your machine, what leaves it, and to whom"
last_validated: 2026-10-11
---

# Orbit Data Handling

Orbit is locally installed software. It stores its state on your disk and has
no Orbit-hosted service, account, or automatic telemetry endpoint. Orbit does
not send your code, tasks, prompts, or logs to the project unless you choose
to share them, for example in an issue or pull request.

This page is written for the person who has to answer a security or procurement
questionnaire about Orbit. It states what Orbit stores, what it sends, and where
the compliance boundary actually sits.

## The one-paragraph answer

All Orbit state lives under `~/.orbit/` and `<repo>/.orbit/` on the host that
runs it. `<repo>/.orbit/` is per-user checkout state (gitignored in full); it
is not a repository artifact. Orbit has no background network traffic. An
explicit `orbit update` can fetch release metadata and binaries from GitHub;
`orbit plugin add` can fetch a digest-pinned HTTPS archive with `curl` or clone
a configured Git source. Agent CLIs, `git`, `gh`, and `ssh` are separate
processes that may send data using accounts and services you configure.
Whether an organization or provider acts as a controller or processor under
GDPR, or as a HIPAA business associate, depends on the actual data, purposes,
and services involved; this page does not make that legal determination. SOC 2
is an attestation framework for service organizations, not a status that can
be ruled in or out for locally installed software in general.

## What Orbit stores, and where

The full inventory, with backup and restore guidance, is in
[state-and-backup](./runbooks/state-and-backup.md). The parts that matter for a
data-handling review:

| Data | Location | Notes |
| --- | --- | --- |
| Task bundles (titles, descriptions, plans, review threads, status) | `~/.orbit/tasks/workspaces/<ws-id>/<task-id>/` | Authoritative. Plain files on disk; may contain whatever you or an agent wrote into a task. |
| Audit events, job runs, step checkpoints, routine state | `~/.orbit/orbit.db` (SQLite) | Authoritative history of what each agent invocation did. |
| Redacted agent output blobs | `<repo>/.orbit/state/audit/blobs/` | Content-addressed; secrets are redacted at write time (see below). |
| Process logs | `~/.orbit/state/logs/orbit.jsonl`, `orbit-agent.jsonl` beside it | JSONL, rotated locally; secret-looking values are redacted before reaching the sink. See [logging](./runbooks/logging.md). |
| Lexical task index | `<repo>/.orbit/state/semantic.db` | Local SQLite FTS5 index; regenerable with `orbit search reindex`. No model or network request. |
| Worktrees | `<repo>/.orbit/state/worktrees/` | Scratch; regenerable. |
| Machine identity (`machine.id`, `machine.name`, `machine.task_prefix`) | `~/.orbit/config.toml` `[machine]` | A locally generated stable identifier. It is never transmitted to the Orbit project. |
| Workspace registry, runtime config, resource overrides | `~/.orbit/config.toml`, `workspaces.json`, `resources/` | Host-global configuration only. |
| Workspace config, routines, auto-tasks, resources | `<repo>/.orbit/config.toml`, `routines/`, `auto_tasks/`, `resources/` | Per-user checkout settings. Seeded by `orbit workspace init`; not committed. Operator settings for shipped auto-tasks live in `auto_tasks/.orbit-auto-task-settings.json`. |

Nothing in this table is synchronised anywhere by default. If you want task
history to leave the machine, you opt in explicitly through
[task publication](./runbooks/task-publication.md) to a Git repository you
choose, or through federated MCP over SSH to hosts you configure.

## What leaves the machine

### Initiated by Orbit itself

| Traffic | When | Destination | What is sent |
| --- | --- | --- | --- |
| Release check and binary download | Only when you run `orbit update` | `api.github.com` / `github.com/constellation-works/orbit/releases` | HTTPS requests for release metadata and selected assets. The `User-Agent` identifies the Orbit CLI version; task and repository content are not included. |
| Plugin archive fetch | When `orbit plugin add` or `orbit plugin upgrade` uses an HTTPS archive source | The configured archive URL | The archive is fetched with `curl`; Orbit requires and checks a pinned SHA-256 digest before unpacking it. |
| Git plugin source | When `orbit plugin add` or `orbit plugin upgrade` uses a `git+` source | The configured Git remote | Orbit runs Git to clone or fetch the requested source and ref. |
| Task publication | When you explicitly publish tasks to a bound Git repository | The repository you chose | The selected task bundles and history are pushed through Git. |

Every Orbit crew dispatches through a provider CLI. Selecting an HTTP-only
provider such as `openai_compat` fails structurally before making a request.

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

| Framework or question | What this page establishes | What still needs assessment |
| --- | --- | --- |
| **[GDPR](https://eur-lex.europa.eu/eli/reg/2016/679/oj)** | Orbit has no hosted service or automatic project telemetry; task and repository data are processed locally and may be sent by configured tools. | Determine the parties' roles from the actual data, purposes, and provider services. Review provider terms and any required data-processing agreements. |
| **[HIPAA](https://www.hhs.gov/hipaa/for-professionals/privacy/guidance/business-associates/index.html)** | The project does not receive PHI automatically. Local Orbit state may contain task or repository content, and configured tools may transmit it. | Business-associate status depends on whether a party performs covered functions involving PHI on behalf of a covered entity or another business associate. Assess the actual services and agreements. |
| **[SOC 2](https://www.aicpa-cima.com/resources/landing/system-and-organization-controls-soc-suite-of-services)** | Orbit has no Orbit-operated hosted service described on this page. | Assess whether a service organization in the data flow needs a SOC 2 report and whether Orbit's local installation falls within your organization's endpoint, access, and change-management controls. |

If a questionnaire asks for a SOC 2 report or DPA from "the vendor," explain
that Orbit is locally installed and has no Orbit-operated hosted service or
automatic project telemetry, then assess the other configured services in the
data flow separately. This page describes Orbit's current behavior; it is not
a legal or compliance determination. See [SECURITY.md](../SECURITY.md) for
project security reporting.

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
- **Air-gapped use works** apart from explicit update downloads and
  whatever your agent CLIs need. Install the binary
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
