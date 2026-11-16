# Exemplars — STD-08 Economy

Worked examples of the rules in [STD-08](STD-08.md), as pointers into the Orbit codebase.
Orbit is the example, not the audience: this file is optional reading, and useful only with
an Orbit checkout.

Paths are relative to `codebases/orbit/`.

- R3, followed — `crates/orbit-core/src/application/notice/limits.rs` (`BRIEF_MAX_ITEMS`,
  `BRIEF_BODY_CHARS`): the notice brief quoted into agent prompts has a fixed item count and
  a per-item cap.
- R3, R6, counter-example — `crates/orbit-core/src/application/resident/steps.rs`, the
  `recent` and `older_checkpoint_notes` lists of the resident brief: five full checkpoints
  plus every older checkpoint's notes, unbounded (ORB-25099).
- R5 — the `fields` projection on `orbit.task.show` and `orbit.task.update`.
- R7, counter-example — a `resident.checkpoint` task comment shown as raw JSON in
  `crates/orbit-web/assets/dashboard/js/task-comments.js` (ORB-25100).
- R10, counter-example — `WorktreeIntegrity` in
  `crates/orbit-engine/src/activity_job/dispatcher.rs`, whose message interpolates the
  serialized JSON diagnostic after the code, shown raw for each failed run on the
  dashboard's run list.
