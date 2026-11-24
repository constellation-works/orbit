macro_rules! delivery_actions {
    ($collect:ident, $state:tt) => {
        $collect! {
            $state;
            core {
                ContextConflictCheck = 7 => "context_conflict_check",
                GateStarvationDefer = 13 => "gate_starvation_defer",
                PipelineSuccessGuard = 18 => "pipeline_success_guard",
                PromoteAgentMain = 23 => "promote_agent_main",
                ReleaseLocks = 27 => "release_locks",
                ReserveLocks = 30 => "reserve_locks",
                ResolveDeliveryJob = 31 => "resolve_delivery_job",
                RevertOnRed = 38 => "revert_on_red",
                ValidateBundles = 44 => "validate_bundles",
            }
            engine {
                CandidateResume = 0 => "candidate_resume",
                CandidateValidate = 1 => "candidate_validate",
                ClaimCandidateCarry = 2 => "claim_candidate_carry",
                ClaimHandoff = 3 => "claim_handoff",
                ClaimValidate = 4 => "claim_validate",
                GitCommit = 7 => "git_commit",
                GitMerge = 8 => "git_merge",
                GitPush = 9 => "git_push",
                GitRebase = 10 => "git_rebase",
                HandoffLand = 11 => "handoff_land",
                PrFailureHandoff = 13 => "pr_failure_handoff",
                PrOpen = 14 => "pr_open",
                PrPrepare = 15 => "pr_prepare",
                PrChecksWatch = 16 => "pr_checks_watch",
                PrComplete = 17 => "pr_complete",
                PrPromote = 18 => "pr_promote",
                TaskComplete = 19 => "task_complete",
                UpdateTask = 20 => "update_task",
                WorktreeSetup = 22 => "worktree_setup",
            }
        }
    };
}

pub(super) use delivery_actions;
