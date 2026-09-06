---
type: design
summary: "Spec: Filesystem Profile Resolution"
tags: ["policy-sandbox"]
last_validated: 2026-09-05
---

# Spec: Filesystem Profile Resolution

`PolicyDef::effective_profile` and `PolicyDef::check_path` are the load-bearing functions for every filesystem allow/deny decision in Orbit. This spec names the invariants those functions must preserve and the failure modes callers must handle.

## Why This Exists

The resolution algorithm has multiple layered transformations (lookup, normalization, deny injection, last-match-wins evaluation) and a special-case fallback for the implicit `unrestricted` profile. Without a prescriptive spec, future changes to any one layer can break a property that another layer relies on.

## Resolution Invariants

- **Schema acceptance.** Only `schemaVersion: 2` policies are accepted. v1 is rejected at load time with an explicit migration message that names `spec.denyRead`, `spec.denyModify`, and `spec.fsProfiles`.
- **Profile lookup.** `effective_profile(profile_name)` returns the named profile if present. If absent and `profile_name == "unrestricted"`, it synthesizes `FsProfile { read: ["./**"], modify: ["./**"] }`. Any other absent name returns `OrbitError::InvalidInput`.
- **Rule normalization.** Every rule is trimmed, has backslashes converted to forward slashes, has leading `./` stripped, and is rejected if it is `~`/`~/…`, contains parent traversals, or is absolute. The normalizer also compiles the rule to its glob-equivalent regex; a rule that fails to compile is rejected at load.
- **Deny injection.** Every entry of `denyRead` is appended to the resolved profile's `read` list as `!<rule>`. Ordinary `denyModify` entries append to `modify` as `!<rule>`. A `denyModify` entry already beginning with `!` is an explicit host-policy exception: it is resolved after its enclosing deny, but only through the selected profile's existing positive coverage, with nested profile rules replayed in order so profile negatives still narrow it. Injection happens after profile lookup so the implicit `unrestricted` profile is also subject to global boundaries.
- **Validation invariants.**
  - Profile names are non-empty.
  - A positive `modify` rule is covered by at least one positive `read` rule in the same profile (`rule_covers_path_rule`).
  - A profile rule that exactly equals a global `denyRead` or `denyModify` entry is rejected.
  - `denyRead` rules are also treated as `denyModify` for the validation cross-check: a profile cannot grant modify on a path that is globally read-denied.
  - `denyRead` exceptions are rejected. A `denyModify` exception must be an exact path or `<path>/**` subtree strictly contained by an earlier deny in the same policy.
- **Merge contract.** `PolicyDef::merged(global, workspace)` overrides global `fsProfiles` by name with workspace entries, accumulates global `denyRead` / `denyModify` with workspace additions (deduplicated), prefers the workspace description when set, and re-runs `validate` on the merged result. A workspace modify exception must be contained by a host exception; it cannot expand the host exception surface. Workspace denies remain later and therefore can narrow a host exception.
- **Authority source.** Modify exceptions intersect `FsProfile::modify`; they do not create authority for read-only/narrow profiles and are never derived from task `context_files`.

## Evaluation Invariants

- **Path normalization.** Caller-supplied paths are normalized via `normalize_glob_path`: trim, slash-flip, strip leading `./`, and canonicalize redundant separators and `.` components. Absolute paths, `~`-anchored paths, and parent-directory components are rejected after backslash normalization.
- **Empty rule list.** If the operation's rule list is empty after deny injection, the decision is `allowed = false` with `matched_rule = "[]"`.
- **Rule walk.** The evaluator walks the rule list in order and tracks the most recent match. The decision uses the *last* match's negation flag: positive match → allow, negated match → deny.
- **Empty positive set.** If the rule list contains no positive rules (only negated rules), the decision is `allowed = false` with `matched_rule = "[]"`.
- **No matching rule.** If positive rules exist but none match, the decision is `allowed = false` with `matched_rule = "<no matching rule>"`.
- **Matched-rule reporting.** A positive match reports the original rule string in `matched_rule`. A negated match reports the inner pattern (without the leading `!`) and surfaces as `allowed = false`. There is no separately persisted negation flag on `FsCheckResult`, `FsPolicyEvaluation`, or `FsCallEvent` — the only structural signal that a match was a deny is the `allowed = false` value. Audit consumers that need to distinguish "denied by an explicit deny rule" from "denied because no rule matched" must inspect `matched_rule` against the policy's deny lists themselves.

## Glob Translator

- **Supported syntax:** `*` (single-segment wildcard, anchored to `[^/]*`), `**` (cross-segment wildcard, anchored to `.*`), `**/` segment (anchored to `(?:.*/)?`), `?` (single character within a segment, anchored to `[^/]`), `<prefix>/**` directory-subtree match (anchored to `^<prefix>(?:/.*)?$`).
- **Unsupported syntax:** character classes (`[abc]`), brace expansion (`{a,b}`), POSIX bracket expressions, and escape sequences are not interpreted as glob operators; they are matched literally. `**` may still be combined with other supported operators, including a `**/` segment followed by another `**`.
- **Anchoring.** Compiled regexes are anchored at both ends (`^…$`). Partial matches do not satisfy a rule.

## Failure Modes

- **Profile missing.** `effective_profile("unknown")` (where the policy does not define `unknown` and the name is not `unrestricted`) returns `OrbitError::InvalidInput`. Callers must treat this as a configuration error, not a deny.
- **Rule normalization failure.** A rule that escapes the workspace, is empty, or fails to compile to a regex returns `OrbitError::InvalidInput` at validation or resolution time. Loaders must surface this to the user; runtimes must treat it as a stop-the-world error rather than falling back to deny-all.
- **Invalid modify exception.** A non-subtree exception, an exception without an earlier strict enclosing deny, or a workspace exception outside the host surface returns `OrbitError::InvalidInput`; callers must not drop the exception and continue with a broader policy.
- **Workspace canonicalization failure.** `PolicyEngine::check_resolved` falls back to the supplied workspace root when root canonicalization fails. A path that cannot be expressed workspace-relative is returned as an outside-workspace denial, which is conservative but does not distinguish "workspace deleted" from "path actually outside."
- **Empty `read` rule list.** A profile authored without read rules denies every read with `matched_rule = "[]"`. This is almost always a misconfiguration but is treated as a valid (if useless) profile.

## Migration Rules

- New profile fields must extend `FsProfile` and `ResolvedFsProfile` together; the resolver and the validator both consume `ResolvedFsProfile`.
- New deny categories (e.g., `denyExec`) must be injected as negated rules into a corresponding rule list rather than evaluated as a separate pass; this preserves the single-walk evaluation contract.
- Schema version bumps must reject the previous version explicitly at load and name the migration in the error message, the same way v1 → v2 currently does.
- The `denyModify` exception syntax extends schema v2; policies without exceptions need no migration. Installing new shipped defaults is not enough by itself: run `orbit init` to refresh the machine-global policy assets before validating live `orbit policy check` and sandbox mount plans. Do not use `--force` merely to refresh defaults.

## Agent Signature

Last revised by codex / gpt-5.6 for [ORB-10560].
