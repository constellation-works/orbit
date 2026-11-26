#![expect(
    clippy::arbitrary_source_item_ordering,
    reason = "macro_rules! ci_actions must precede its pub(super) use re-export in lexical macro scope"
)]

macro_rules! ci_actions {
    ($collect:ident, $state:tt) => {
        $collect! {
            $state;
            core {
                ConsolidateCodeScanningTasks = 6 => "consolidate_code_scanning_tasks",
                FileCiFailureTasks = 10 => "file_ci_failure_tasks",
                FileDependabotAlertTasks = 11 => "file_dependabot_alert_tasks",
            }
            engine {
                CollectCiEvidence = 5 => "collect_ci_evidence",
                CollectDependabotAlerts = 6 => "collect_dependabot_alerts",
            }
        }
    };
}

pub(super) use ci_actions;
