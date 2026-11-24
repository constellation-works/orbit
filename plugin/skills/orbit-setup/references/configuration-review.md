# Configuration: review settings

## Contents

- [Common settings: review](#common-settings-review)
- [Review baseline commands and owed host evidence](#review-baseline-commands-and-owed-host-evidence)
  - [Set the policy](#set-the-policy)
  - [Verify the effective policy and its effect](#verify-the-effective-policy-and-its-effect)

## Common settings: review

| Key | Purpose |
|---|---|

| `review.timing` | Review before landing, one layer at most (default `off`). `before_pr` holds PR creation for a fresh reviewer that fixes what it finds. `before_landing` opens the PR first and reviews it while hosted CI runs; it merges only at the reviewed head, and any other outcome leaves the PR open with the task in `review` (PR route only). A run captures it at submission. After-landing review is not a key: toggle the `delivery-code-review` auto-task (`orbit auto-task toggle delivery-code-review on`). |
| `review.before_landing_hosts` | Machine ids (`hm_...`) whose claimed leaves review their open PR before it lands, while this owner's own deliveries keep `review.timing` off. Probe and pull admission resolve the caller's label and the claim captures it. Ignored when `review.timing` is already `before_landing`. Loading fails while `review.timing` is `before_pr` and the list is non-empty. The label is caller-chosen, so this is policy, not a security boundary. Default empty. |
| `review.minutes` | Wall-clock limit for one candidate's before-PR or before-landing review (1..=1440, default 30). Each candidate gets one review; a changed candidate, such as a completion rebase, is a new one. |
| `review.baseline_commands` | Default `[]`; commands the owner trusts review settlement to rerun on the host for a reviewer's red-base claim, in addition to required validation commands. Entries are trimmed strings; blank entries are dropped. A configured command is binding during settlement. |
| `[[review.host_evidence]]` | Default `[]`; owner policy for checks owed on claimed reviews when changed paths or listed task tags match. This TOML array replaces the whole global list at workspace scope; see [review baseline commands and owed host evidence](#review-baseline-commands-and-owed-host-evidence). |
| `workflow.purpose_crews.review` | Crew or pool for after-landing review tasks the `delivery-code-review` auto-task mints. Before-PR and before-landing review use `workflow.purpose_crews.pre_landing_review` when non-empty, falling back to this pool when it is unset or empty. A pool such as `["sol", "grok"]` (or weighted `["sol:3", "grok:1"]`) draws one crew per review, skipping disabled or provider-limited crews and preferring one that did not implement the work. |

`operation.review_policy` and `operation.review_minutes` are deprecated: they
still load, translated with a warning (`before-pr` → `review.timing = "before_pr"`;
`after-landing` no longer enables `delivery-code-review` (its own `enabled` flag decides);
`none` → `review.timing = "off"`), and a later release makes them errors. Move
them to `[review]` and the auto-task flag.

`operation.review_reviewer_starts` and
`operation.review_repair_cycles` are unknown settings: load ignores them with
the generic unknown-key warning; delete them.
`orbit config show --all` reports the review timing and the after-landing switch with
their sources; `--json` includes the same `review` object, and the default text view prints the `review.*` rows without that summary.

## Review baseline commands and owed host evidence

These settings govern the checks review settlement may rerun and the checks an
owner may owe after a claimed review. They do not turn on before-PR or before-landing review; set
`review.timing` separately when that review workflow is wanted.

### Set the policy

`review.verify_findings` was removed with the finding-verification routine in
this release. It loads with a deprecation warning and is ignored until
2026-12-07, and nothing holds a review-filed bug as
`unverified_review_finding`. An invalid finding is still rejected by the task
pilot's `finding_invalid` result or an implementer's `finding-invalid.json`,
which record a REJECTED verdict.

`review.baseline_commands` is an array of owner-trusted commands, default
`[]`. Review settlement may use them, along with
`workflow.required_validation_commands`, to rerun an eligible failed check on
the host and determine whether a reviewer's red-base claim also fails on the
base. The reviewer cannot authorize an arbitrary host command by naming it in
the report.

If a listed command cannot be confirmed on the base, the review
settles incomplete; the list is therefore binding policy, not a hint. These
commands also count as required validation gates an implementer may hand off
when its sandbox deferred only an admitted sandbox-dependent test.

`review.baseline_commands` can be written with `orbit config set`, for example:

```sh
orbit config set review.baseline_commands '["cargo test -p service --test api"]'
```

`[[review.host_evidence]]` has no `config set` key. Add its array-of-tables
directly to `<workspace>/.orbit/config.toml` or `~/.orbit/config.toml`.
The workspace list replaces the entire global list, including when set to
`[]`. Replace the example paths, package and target with ones that exist in
your project. A `codeql` command the built-in fulfilment runs stays on the
admitted script below; do not replace it with another program.

```toml
[review]
baseline_commands = ["cargo test -p service --test api"]

[[review.host_evidence]]
kind = "host_sandbox_test"
name = "Linux sandbox tests"
paths = ["src/sandbox/**", "tests/sandbox/**"]
os = "linux"
command = "cargo test -p service --test sandbox_tests"
artifact = "evidence/linux-sandbox.json"

[[review.host_evidence]]
kind = "codeql"
name = "Linux code analysis"
paths = ["src/**"]
os = "linux"
command = "scripts/codeql-rust-local.sh --ram 16384 codeql/rust-queries:codeql-suites/rust-security-extended.qls"
artifact = "evidence/linux-code-analysis.json"
hosted_check = "CodeQL / Analyze"
```

Each rule admits only these fields: `kind`, `name`, `paths`, `os`,
`command`, `artifact`, and optional `hosted_check` and `tags`; unknown
fields are rejected. `kind` is exactly `codeql` or `host_sandbox_test`, `os` is
`linux` or `macos`, `name` and `command` must be non-empty, and `paths`
must contain at least one valid workspace-relative glob. The `artifact` must
be a unique relative `.json` path across rules.

A `host_sandbox_test`
command must use an admitted form:
`cargo test -p <package> --test <target> [<one-filter>]` or
`cargo test -p <package> --lib [<one-filter>]` (`--package` may stand in
place of `-p`, and a single `--` may precede the one filter). Shell
metacharacters and other command shapes are refused. `--lib` is not
combined with `--test` or any other cargo option.

A `codeql` rule the built-in fulfilment runs must name
`scripts/codeql-rust-local.sh`, then optional `--ram <mib>` and
`--toolchain <version>`, then exactly one query selector, as in the example.

Download that script from
<https://raw.githubusercontent.com/constellation-works/orbit/v0.54.0/scripts/codeql-rust-local.sh>
and save it at `scripts/codeql-rust-local.sh` in the workspace, then make it
executable (`chmod +x scripts/codeql-rust-local.sh`, committed as mode 100755):
the fulfilment executes that path directly, so a copy saved without the
executable bit cannot run. Its default extractor toolchain is Rust 1.99.0 plus
rust-src, confirmed for CodeQL 2.27.5. For another CodeQL bundle, pass
`--toolchain <version>`; do not substitute the host's installed stable Rust.

Configuration accepts any non-empty `codeql` command, and a custom command
is not fulfilled by the built-in path: the hold stays, and the refusal names
this contract and that download.

A rule applies when the candidate changes a path matching `paths`, or when
the claimed task carries any tag listed in optional `tags` (compared
case-insensitively; a blank tag fails load), such as
`tags = ["security", "security-review"]` for a check every security task
owes whatever it changed. Followers need a matching Orbit build to claim
work under a rule that lists tags.

A `codeql` rule is owed when the claimed review host has a different OS from
the rule's `os`, and the owner fulfils it on the named OS. A
`host_sandbox_test` rule is owed on a host of its named OS and is run outside
the agent sandbox at settlement. Windows is not a valid evidence OS. A
`hosted_check` is allowed only on a `codeql` rule with `os = "linux"`, and
it must be non-empty and name a pull-request check that runs exactly the rule's
`command` on every candidate.

For a before-landing claim on a different known
OS, this lets the owner treat that CodeQL check as owner-owed evidence and
wait for explicit success on the reviewed head before landing. A skipped or
neutral check is not success. When a CodeQL check is owed on another OS,
without `hosted_check` the review remains held until the owner supplies the
required evidence.

At claim admission, the owner captures validation gates including
`review.baseline_commands`; when the claim includes a before-PR or
before-landing review, it also captures `review.host_evidence`. The claimed
leaf follows those owner snapshots, not its own config. Updating owner policy
affects new claims. If the captured policy no longer matches the owner's
current policy at acceptance, the handoff fails closed and needs a fresh claim.

### Verify the effective policy and its effect

Run `orbit config show --all` to inspect the effective values and provenance
for `review.baseline_commands`. Use
`orbit config keys --json` to confirm which settings the installed version
supports; `review.host_evidence` is TOML-only and is not a `config set` key.
Check the global and workspace config files when reviewing host-evidence
rules, since a workspace list replaces the global one, and run `orbit doctor`
to catch configuration errors before a claim.

For a claimed review, inspect the
task's `review-gate.json` artifact with
`orbit task artifact get <task-id> review-gate.json --out <file>`:
`owed_evidence` records checks the reviewer did not run and are pending host
fulfilment, not failed checks. Compare with validation records or
host-evidence outcomes: an executed command with a failed result is a failed
check and follows normal review recovery.

Use `orbit config keys` to distinguish fixed registry keys from settings
authored as TOML. Read-only identity keys are listed and refused by
`config set`. Named crew fields are also settable as
`crews.<name>.<field>` (for example `orbit config set crews.sol.effort high`)
even though those keys are not listed by `orbit config keys`. Creating a crew
still requires a `[crews.<name>]` table with `model` and `provider`.

Set
workspace ship mode with
`orbit workspace init --ship-mode pr|local`, or rebind a registered workspace
without re-initializing it with `orbit workspace ship-mode pr|local` (no
argument prints the current mode); verify with `orbit workspace show`. PR mode
needs a Git remote on a forge host: when no remote names a network host, ship
and the drain refuse untagged tasks before dispatch and `orbit doctor` warns on
its `forge-remote` row.

Base branch and ship mode govern source delivery,
not task snapshot publication.
