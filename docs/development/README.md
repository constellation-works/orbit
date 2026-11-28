# Development reference

Find guidance in [Validation and CI](validation-and-ci.md), [Hosted CI](hosted-ci.md), [Processes](processes.md), [QA](qa.md), [Testing](testing.md), [Browser checks](browser-checks.md), and [Toolchain](toolchain.md).

## Run build provenance

New job runs store `build` with the compiled Orbit `version`, full `source_commit`,
and `dirty` flag. `orbit run show --json` and `orbit run history --json` expose
that record at `.run.build` and `.runs[].build`, respectively. Admission records the submitting build; the winning first
start replaces it with the worker's build, and subsequent updates preserve it.
Historical rows remain `null`, including after migration or inspection.

`orbit-cli/build.rs` embeds Git identity while compiling the `orbit` binary,
watching worktree HEAD, branch refs and source changes so an incremental
rebuild refreshes it. `main` registers that identity before any store opens.
The embed sits on the leaf binary so a provenance refresh recompiles `orbit`
alone. Processes that never register (other crates' tests and examples)
record unknown provenance: no source commit and `dirty: true`. Builds from
source archives without Git record a null source commit. Dirty builds
identify their base commit but cannot prove a fix was included. Deployment
analysis must use clean run builds and commit ancestry, never the current
checkout, semantic version order, or today's installed binary.
