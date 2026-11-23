# Author or inspect an overridden job

## Custom jobs and resource overrides

Use `orbit job show <id>` to inspect effective installed job definitions and
the activity each step runs before changing them.
Workspace resource overrides can shadow shipped global resources, so the
binary's version alone does not prove which pipeline ran. `orbit workspace sync
--check` reports managed-resource drift; customized files are preserved for
deliberate reconciliation.

A version 2 activity job uses `schemaVersion: 2`, `kind: Job`, `metadata.name`,
and a `spec` with `default_input` and ordered `steps`. A simple step names an `id`, a
`target: activity:<name>`, and `default_input`. Templates can reference
`input.<name>` and `steps.<step-id>.output.<field>`. The shipped jobs demonstrate
conditionals, loops, retries, and recovery activities. Copy an installed example
that matches the intended operation; validate the effective catalog before
submitting it.

A routine can only target a job, not an activity directly.

An agent step's brief, selected crew, filesystem profile, allowed tools, and
completion envelope are separate contracts. A successful provider exit alone
is insufficient when the step requires structured completion output. Keep
required tools exact and minimal, and declare read-only filesystem profiles
explicitly for inspection work. File meaningful failures rather than treating
an empty/malformed agent response as successful completion.
