//! Preserve the persisted pipeline field order across Rust state grouping.
//!
//! A derived flattened serializer would move the forge hold and drain controls
//! across intervening general fields. Keep the historical order and omission
//! rules so rewriting a pre-grouping state.json leaves its bytes unchanged.

use serde::ser::{Serialize, SerializeStruct, Serializer};

use super::PipelineState;

impl Serialize for PipelineState {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // Count and emit from one ordered list; the predicates are pure.
        macro_rules! fields {
            ($($name:ident: $value:expr => $skip:expr),* $(,)?) => {{
                let len = 0 $(+ usize::from(!$skip))*;
                let mut state = serializer.serialize_struct("PipelineState", len)?;
                $(
                    if !$skip {
                        state.serialize_field(stringify!($name), $value)?;
                    }
                )*
                state.end()
            }};
        }

        fields! {
            run_id: &self.run_id => false,
            job_id: &self.job_id => false,
            initial_input: &self.initial_input => false,
            pipeline: &self.pipeline => false,
            step_outputs: &self.step_outputs => false,
            compound_outputs: &self.compound_outputs => self.compound_outputs.is_empty(),
            pipeline_patches: &self.pipeline_patches => false,
            step_output_pointers: &self.step_output_pointers => self.step_output_pointers.is_empty(),
            step_states: &self.step_states => false,
            next_step_index: &self.next_step_index => false,
            previous_step_state: &self.previous_step_state => false,
            iteration: &self.iteration => false,
            waiting_on_deps: &self.waiting_on_deps => self.waiting_on_deps.is_none(),
            waiting_on_locks: &self.waiting_on_locks => self.waiting_on_locks.is_none(),
            child_dispatches: &self.child_dispatches => self.child_dispatches.is_empty(),
            drain_worker_limit: &self.pull_drain.drain_worker_limit => self.pull_drain.drain_worker_limit.is_none(),
            drain_admissions_stop: &self.pull_drain.drain_admissions_stop => self.pull_drain.drain_admissions_stop.is_none(),
            drain_window_extension: &self.pull_drain.drain_window_extension => self.pull_drain.drain_window_extension.is_none(),
            drain_cancel: &self.pull_drain.drain_cancel => self.pull_drain.drain_cancel.is_none(),
            task_cancellation_policy: &self.task_cancellation_policy => self.task_cancellation_policy.is_none(),
            pull_crew_preflight: &self.pull_drain.pull_crew_preflight => self.pull_drain.pull_crew_preflight.is_none(),
            pull_auth_recovery: &self.pull_drain.pull_auth_recovery => self.pull_drain.pull_auth_recovery.is_empty(),
            pull_single_pass: &self.pull_drain.pull_single_pass => self.pull_drain.pull_single_pass.is_none(),
            pull_window_close: &self.pull_drain.pull_window_close => self.pull_drain.pull_window_close.is_none(),
            pull_breaker: &self.pull_drain.pull_breaker => self.pull_drain.pull_breaker.is_none(),
            pull_admission_fit: &self.pull_drain.pull_admission_fit => self.pull_drain.pull_admission_fit.is_none(),
            drain_last_pass: &self.pull_drain.drain_last_pass => self.pull_drain.drain_last_pass.is_none(),
            drain_last_forecast: &self.pull_drain.drain_last_forecast => self.pull_drain.drain_last_forecast.is_none(),
            drain_approvals: &self.pull_drain.drain_approvals => self.pull_drain.drain_approvals.is_none(),
            failure_activity_checkpoint: &self.failure_activity_checkpoint => self.failure_activity_checkpoint.is_none(),
            rebase_recovery_checkpoints: &self.rebase_recovery_checkpoints => self.rebase_recovery_checkpoints.is_empty(),
            activity_crew_draws: &self.activity_crew_draws => self.activity_crew_draws.is_empty(),
            completion_deadlines: &self.completion_deadlines => self.completion_deadlines.is_empty(),
            final_recovery: &self.final_recovery => self.final_recovery.is_none(),
            forge_hold: &self.pull_drain.forge_hold => self.pull_drain.forge_hold.is_none(),
            forge_hold_expired_at: &self.pull_drain.forge_hold_expired_at => self.pull_drain.forge_hold_expired_at.is_none(),
            trigger: &self.trigger => self.trigger.is_none(),
            env_pass_unset: &self.env_pass_unset => self.env_pass_unset.is_empty(),
            root_cause: &self.root_cause => self.root_cause.is_none(),
            updated_at: &self.updated_at => false,
        }
    }
}
