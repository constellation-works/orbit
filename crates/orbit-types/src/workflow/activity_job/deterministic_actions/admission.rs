macro_rules! admission_actions {
    ($collect:ident, $state:tt) => {
        $collect! {
            $state;
            core {
                AdoptOrphanedDeliveries = 0 => "adopt_orphaned_deliveries",
                ApplyBlockedTaskRecovery = 1 => "apply_blocked_task_recovery",
                ApplyTaskPilotResults = 2 => "apply_task_pilot_results",
                AwaitApprovalPilot = 4 => "await_approval_pilot",
                ClassifyWorkspaceAutoTasks = 5 => "classify_workspace_auto_tasks",
                DrainWindow = 9 => "drain_window",
                ListBacklogTasks = 16 => "list_backlog_tasks",
                PrepareBlockedTaskRecovery = 20 => "prepare_blocked_task_recovery",
                PrepareEscalationTriage = 21 => "prepare_escalation_triage",
                PrepareTaskPilot = 22 => "prepare_task_pilot",
                PullRefill = 24 => "pull_refill",
                RecordProposedApprovals = 25 => "record_proposed_approvals",
                ResolveWorkspaceShipInput = 32 => "resolve_workspace_ship_input",
                ScanUnresolvedWork = 39 => "scan_unresolved_work",
                SelectProposedApprovals = 40 => "select_proposed_approvals",
            }
            engine {}
        }
    };
}

pub(super) use admission_actions;
