---
type: design
summary: "Spec: Artifact Write Redaction"
tags: ["auditability"]
last_validated: 2026-09-20
---

# Spec: Artifact Write Redaction

Orbit artifact tools persist repo-backed YAML, markdown, and JSON. Their write boundary sanitizes selected input fields after `OrbitBuiltinAction` is known and before typed params are built. The sanitizer is action-keyed rather than field-blind, so structural IDs, statuses, tags, and artifact blobs keep their native validation rules.

This is the author-facing inventory for the shipped artifact-write redactor. Redaction is a backstop: authors should summarize terminal diagnostics and environment dumps instead of pasting them verbatim whenever the source can be reduced safely.

## Field Policy

| Tool | Free text (`redact_all` + `redact_home_dir`) | Path-only (`redact_home_dir`) | Skip |
|------|----------------------------------------------|-------------------------------|------|
| `orbit.adr.add` / `orbit.adr.restore` / `orbit.adr.update` | `title`, `body` | - | status, owner, related ids/features/tasks, legacy ids |
| `orbit.adr.supersede` | - | - | `old_id`, `new_id` |
| `orbit.task.add` | `title`, `description`, `plan`, `acceptance_criteria[]`, `comment` | `context_files[]`, `context`, `external_refs[].url` | workspace, ids, enums, dependency/relation targets, crew, tags |
| `orbit.task.update` | `title`, `description`, `plan`, `execution_summary`, `acceptance_criteria[]`, `comment` | `context_files[]`, `context` | provenance/status/identity fields, tags, raw artifacts |
| `orbit.task.reject` | `note`, `comment` | - | `id` |
| `orbit.friction.add` | `body` / `description` | - | `model`, `during_task`, tags |
| `orbit.friction.update` | `body` | - | `id`, status, tags, `rehome_to` |
| `orbit.friction.rehome` | - | - | `id`, `to_workspace`; the moved body was already redacted when it was written |
| `orbit.auto_task.add` / `orbit.auto_task.update` | `description`, `template.title`, `template.description`, `template.acceptance_criteria[]` | - | name, schedule, dedupe, template enums/tags |
| `orbit.docs.add` | - | - | DocsAdd only registers a validated repo-relative path; it does not persist document content. |

Task and friction tags are taxonomy fields and pass through verbatim.

The table establishes the artifact boundary: ADRs, tasks, frictions, and auto-task definitions are covered on their listed write operations. `DocsAdd` makes an explicit no-redaction decision because it only registers a checked path; registered docs remain ordinary repository files rather than a tool mutation primitive. Session-log writes are no longer a public tool mutation, so they are not in this inventory ([ORB-11097]).

`policy_for_action` exhaustively matches `OrbitBuiltinAction`. Adding any builtin action therefore fails to compile until it receives either a field policy or an explicit no-redaction decision, instead of falling through to an unredacted default.

## Pattern Set

Free-text fields first replace values of live environment variables whose names are credential-shaped (`SECRET`, `TOKEN`, `PASSWORD`, `API_KEY`, `PRIVATE`, `CREDENTIAL`, `COOKIE`, `SESSION`, `BEARER`, or `AUTH`). The shared structural patterns then mask:

- JSON and raw header forms of `Authorization`, `x-api-key`, and `api_key`; bearer values; and `key` query parameters
- OpenAI, Google, GitLab, GitHub, AWS, npm, and Slack credential token shapes, AWS secret assignments, and URI user-info passwords
- OpenSSH `SHA256:` public-key fingerprints
- comments on serialized OpenSSH public keys, and the key descriptor/comment on `ssh-keygen -l` and canonical verbose key-offer lines
- host/address identifiers only in canonical OpenSSH `Connecting to`, `Authenticating to`, and `Authenticated to` diagnostics

SSH replacements are class-labelled as `[REDACTED_SSH_FINGERPRINT]`, `[REDACTED_SSH_KEY_COMMENT]`, or `[REDACTED_SSH_HOST]`. Host matching is deliberately contextual rather than a general hostname rule. Commit SHAs, lowercase content hashes, run ids, task ids, worktree paths, repository names, and model strings are not secret classes and are preserved unless they literally contain a separately recognized credential or live sensitive environment value. [ORB-10591]

## Refuse vs Mask

Free-text fields reject a value that is exactly one high-confidence credential token, using the shared high-confidence provider-token, secret-assignment, and credentialed-URI patterns.

The rejection is a typed `OrbitError::SensitiveInput` and never includes the token value. The same token shapes embedded in larger prose are masked by the shared redaction module. Path-only fields only normalize HOME-prefixed strings; token-shaped globs and character classes are preserved.

## Response and Audit Contract

Covered mutating tools add `redactions_applied: bool` and `redactions: [{field_path, redaction_kinds, redaction_classes}]` to object responses. The boolean is `true` only when at least one persisted field changed from the caller's input. The detail list names every changed field, whether environment, structural-pattern, or home-directory normalization acted, and concrete safe classes such as `credential`, `ssh_fingerprint`, `ssh_key_comment`, or `ssh_host`. Re-running a write with already-redacted text is idempotent: no field changes means `redactions_applied: false`, `redactions: []`, and no redaction audit event.

When a field changes, Orbit emits one command-audit row per field. The payload contains only:

- artifact type and id
- field path
- actor
- tool name
- redaction kinds: `env`, `pattern`, `home_dir`
- redaction classes: `sensitive_environment_value`, `home_directory`, `authorization`, `credential`, or the structural SSH classes above

Original and redacted values are not recorded. Tests should inspect these rows through the same backing surface as `orbit audit list --json` per L-0009.

## Non-Goals

- Display-time/read-time redaction.
- Raw task artifact blob redaction through `orbit.task.artifact.put` or task `artifacts` / `upsert_artifacts`.
- General-purpose schema validation in the sanitizer; existing typed parsers keep reporting shape errors.
- Auto-publish, commit, or remote cleanup behavior after a secret has already entered history.
- Automatic sweeps of existing authored records or git history rewriting; see the forward-only decision recorded for [ORB-10591].
