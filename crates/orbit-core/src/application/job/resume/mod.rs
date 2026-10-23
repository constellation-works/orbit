//! [ORB-10470] Resume planning: retry-lineage ownership + checkpoint reuse.
//!
//! A resumed run is not a fresh submission. It re-enters a workflow that a
//! previous attempt already admitted, in a worktree that attempt created,
//! against tasks that attempt stamped with its own `job_run_id`. Three durable
//! facts therefore have to be reconciled *before* the resumed run reaches its
//! delivery tail (F2026-07-121 / F2026-07-122):
//!
//! 1. **Blocked tasks.** A terminal run failure or interruption blocks every
//!    coupled task (`runtime::task::block_on_run_failure`; `workflow_run_failed`
//!    or `workflow_run_interrupted`). The run's own failure handoff can block
//!    the task first (`pr_failure_handoff`, `pr_conflict_blocked`,
//!    `validation_environment_blocked`, `review_gate_escalation`), and
//!    finalization then leaves that block in place because `blocked` is not
//!    blockable again. `blocked` is also outside the workflow-admission
//!    allowlist. If the resumed run replays `worktree_setup`, admission
//!    rejects the very task the resume exists to recover — a catch-22.
//!    Resume readmits a block only when that latest system entry attributes
//!    itself to this lineage. An operator block stays untouched.
//! 2. **Ownership drift.** When checkpoints *are* reused, `worktree_setup` is
//!    skipped, so nothing re-claims the task. Downstream delivery steps keep
//!    consuming the checkpointed `steps.<worktree>.output.job_run_id` as their
//!    batch id, while the task record may have been re-stamped by an
//!    intervening failed attempt in the same lineage. A task bound to a run
//!    outside that lineage — including another machine's claim — is refused
//!    here, before those checkpoints or a validation input are reused.
//!
//! 3. **Delivery stage.** A final-recovery escalation, or a failure-handoff
//!    block attributed to this lineage, can block a task after promotion to
//!    review. Reusing that promotion checkpoint must restore review, rather
//!    than readmitting implementation that completion skips.
//!
//! These are repaired by reconciling against the run's **explicit retry
//! lineage** — the source run, its `retry_source_run_id` ancestors, and the
//! runs descended from them — and never against an unrelated run. A task
//! whose binding (`job_run_id` and `job_run_machine`) is outside that lineage
//! is not resumed and not re-stamped. Planning refuses before any step runs
//! and names the current binding; the next action is to continue that binding
//! on its machine, or to admit a new attempt after an authorized rebind.
//! Resume does not rebind a foreign claim. A legitimate resume re-keys only
//! the pipeline document's run id. Ownership ids inside checkpoints and the
//! copied input stay the batch that created the worktree; rewriting them
//! would make validation name a run that does not own the task. The attach
//! guard in `runtime_host` stays fail-closed for a non-owning run.

mod binding;
mod completion_repair;
mod plan;
mod source_repair;
mod submit;

pub(crate) use plan::ResumePlan;
pub(crate) use source_repair::PublishedResume;
// `resume::PinnedRunDefinition` was the path while this type lived in
// `resume.rs`. Nothing else in the crate names it; the re-export keeps that
// path for callers outside this module.
pub(crate) use binding::shelving_run_id;
pub(super) use binding::task_ids_from_input;
pub(crate) use completion_repair::OUT_OF_RUN_AUTO_FIX_KEY;
#[expect(unused_imports)]
pub(crate) use plan::PinnedRunDefinition;
