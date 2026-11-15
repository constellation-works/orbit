---
id: STD-05-security-boundaries
title: Security boundaries — authority, filesystem containment, child environments, redaction, local network surfaces, supply chain and consent
summary: Normative trust-boundary rules for constellation CLIs and small local services — authority from host-owned state, symlink-safe path containment, private local state, allowlisted child environments, value-based redaction at the durability boundary, loopback surfaces that check Host and Origin, no phoning home, digest-bound consent, pinned and verified supply chain, CSPRNG tokens; follow it in any tool that runs untrusted children, persists state, or listens on a socket.
status: active
tags: [standard, security, sandbox, secrets, redaction, supply-chain, network]
version: 1
created: 2026-09-26
updated: 2026-09-26
last_validated: 2026-09-26
related: [STD-01-cli-surface, STD-02-rust-architecture-and-errors, STD-03-concurrency-and-process-safety, STD-04-testing-and-verification]
---

# Security boundaries

These rules decide where a tool's trust boundaries sit and how it keeps them. A trust
boundary is where input from something the tool does not control reaches something it
protects: an agent subprocess calling back into the tool, a path a plugin names, a browser
page reaching a loopback socket, a dependency pulled at build time. Most rules below come
from Orbit security reviews that found a boundary which only looked like one. The rules are
written for a small CLI or a local service that is not Orbit. Orbit is the worked example,
not the audience. Orbit paths are relative to `codebases/orbit/`.

Three things this standard does not repeat:
- **Secrets in the repository.** Polaris CONVENTIONS §4
  ["Never commit secrets"](../../CONVENTIONS.md#never-commit-secrets) is a standing policy for
  every agent. It covers docs, records, commit messages and filenames. The rules here cover
  what a running program does with secrets.
- **Fail closed.** When a rule below cannot be decided — the authority fact, the path, the
  digest or the owner cannot be verified — the tool refuses. That is STD-02 (fail closed on
  unverifiable security state), and every rule here inherits it.
- **Subprocess lifecycle.** Process groups, termination and reaping are STD-03 §R11–R16, and
  test-fixture environments are STD-03 §R20. The rules here cover what a child is *given*.

The keywords MUST, MUST NOT and SHOULD are normative. A rule marked *review-only* in
[`checks.md`](checks.md) has no automated gate, so a reviewer enforces it.

This standard is a directory. This file holds the binding text: the rules, where they
apply, and how to deviate. Beside it, [`why.md`](why.md) gives the reason for each rule,
[`checks.md`](checks.md) the gate that enforces it, [`exemplars.md`](exemplars.md) worked
examples in Orbit, and [`CHANGELOG.md`](CHANGELOG.md) what changed in each version.

## Rules

### Authority and identity

- **R1.** Authorization, identity and provenance MUST derive from state the host controls and
  the subject cannot rewrite: a host-minted session record, a descriptor the host opened, an
  identifier stamped at write time, file ownership. Environment variables, self-declared
  labels, process ancestry and request payloads are claims, not authority.
- **R2.** A caller's self-reported identity that is worth keeping MUST be stored in its own
  field, apart from the authenticated identity. It never replaces or widens the
  authenticated identity.
- **R3.** Whether a surface lists or hides an operation MUST NOT be used as a permission
  check. Permission is decided by one authorization function that every entry surface calls
  before the operation runs.
- **R4.** An escape hatch that raises a caller's authority MUST be explicit and named, and
  every use MUST be audited with its own provenance, so an override is never
  indistinguishable from an ordinary call.
- **R5.** A guard that the subject can bypass (for example, one enforced against a process
  running as the same OS user) MUST be documented as an accident guard, not a security
  boundary. No other control may be relaxed because that guard exists.

### Filesystem boundaries

- **R6.** Path containment MUST be decided on the physical path, checked against an
  allowlist of roots. Canonicalize a path that exists. For a path that does not exist yet,
  canonicalize the nearest existing ancestor and append the missing names. Never decide on
  the spelled or lexically normalized path alone.
- **R7.** The operation MUST act on the path that was checked. The check and the operation
  share one resolution. A write or create does not follow a symlink in its final component
  (`O_NOFOLLOW` or an equivalent `lstat` check), and it refuses when what it created does
  not resolve to the identity that was validated.
- **R8.** Local state files and directories MUST be created with owner-only modes (`0600`
  for files, `0700` for directories), set explicitly at creation and never left to the
  process umask.
- **R9.** State that grants authority or holds secrets MUST be validated when it is loaded:
  owned by the current user, not group- or world-writable, and not reached through a
  symlink. On failure, the tool refuses to use it, or repairs the modes and warns.

### Process and environment

- **R10.** A child process that is not fully trusted MUST get an environment composed from
  one explicit allowlist: a documented non-credential baseline, the names the operator
  configured, and the names the child's integration declares it needs. The child launches
  from a cleared environment. Never filter the parent environment with a denylist or with
  name or value heuristics.
- **R11.** Privilege-bearing variables in the tool's own namespace (operator overrides,
  claim tokens, callback credentials) MUST NOT reach an untrusted child, even when a
  caller-supplied pass list names them explicitly.
- **R12.** A secret MUST NOT be passed to a child in argv, and MUST NOT sit in an
  environment that children inherit without needing it. Hand a secret over through a private
  file, an inherited descriptor or stdin.

### Secrets and redaction

- **R13.** Redaction MUST run at the durability boundary. Every writer that persists free
  text — log sinks, audit blobs, stored artifacts, persisted error messages — passes its text
  through one redactor before the write. The set of covered writers is an explicit inventory
  that a test checks.
- **R14.** Redaction MUST match secret values, not vocabulary. It masks the live values of
  secret-bearing variables and high-confidence credential shapes (bearer headers, provider
  key prefixes). It never masks an ordinary word because a secret-named variable happens to
  hold it, and never masks part of an ordinary identifier.
- **R15.** A tool that uses the operator's credentials MUST read them where the operator
  keeps them (the provider's own credential store, or a variable the operator names for
  pass-through). It MUST NOT copy them into its own state.

### Network surfaces

- **R16.** A loopback bind MUST NOT be treated as authentication. A local HTTP server
  validates the `Host` header against an approved loopback authority on every route,
  including health and diagnostics, and refuses a missing or unparsable `Host`.
- **R17.** A local HTTP server MUST refuse every state-changing request (`POST`, `PUT`,
  `PATCH`, `DELETE`), and every request that carries an `Origin` header, unless `Origin` is
  a loopback `http` origin that matches the validated `Host`.
- **R18.** A listener that speaks a protocol other than HTTP MUST reject any stream that
  does not open with that protocol's framing before it dispatches anything. That stops a
  browser's HTTP request from being parsed as protocol input.
- **R19.** A network listener SHOULD bind loopback by default and refuse a wider bind
  unless it is requested explicitly. Remote access SHOULD reuse an authenticated transport
  the operator already has (SSH forwarding) rather than add new credentials or a routable
  listener.
- **R20.** A tool MUST NOT make a network request that the operator did not initiate or
  configure. That rules out an update check on startup, usage telemetry and crash reporting
  unless the operator turned them on.

### Supply chain and consent

- **R21.** A consent or grant MUST bind to a digest of the content that was approved, not to
  a name. When the content on disk no longer matches the recorded digest, the grant does
  not apply until the operator approves again.
- **R22.** A registry that accepts names from a less-trusted source (plugins, extensions,
  user definitions) MUST refuse a name that collides with a built-in or with another
  source's entry. It never overwrites or shadows it silently.
- **R23.** Build, CI and install inputs MUST be pinned to immutable identifiers and verified
  when they are fetched: a committed lockfile, CI actions pinned by full commit SHA, and
  downloaded tools and release artifacts checked against a recorded checksum or signature.
- **R24.** Every third-party dependency, including code vendored or copied into the
  repository, MUST have a machine-readable record that advisory monitoring reads.
- **R25.** Every token, nonce or session identifier with security meaning MUST be drawn from
  the operating system's cryptographically secure random source.

## Applies to

- `universal`: R1 to R7, R10 to R14, R20 to R25. Any tool that runs children, touches paths
  from another party, persists text, or has dependencies.
- `cli`: every `universal` rule. A single-binary CLI with no plugins meets R3 to R5, R21 and
  R22 trivially and says so.
- `stateful`: R8, R9, R13 and R15. Any tool that keeps local state across runs.
- `service`: R16 to R19. Any tool that listens on a socket, even on loopback.
- `rust`: the Rust gates in [`checks.md`](checks.md).

## Deviations

An adopting repo that departs from a rule records a decision in its own design docs, citing
`STD-05@1 §Rn` and the reason. It never edits its vendored copy of this standard.

Some deviations are expected, and the decision still needs to be recorded:
- A service meant to be reached over a network departs from `STD-05@1 §R19`. It then needs
  real authentication, and R16 and R17 still apply to any browser-facing route.
- An operator-enabled update check or telemetry is not a deviation from R20, because the
  operator configured it. A default-on check is, and cites `STD-05@1 §R20`.
- A platform without file modes (Windows) meets R8 and R9 with owner-only ACLs.
