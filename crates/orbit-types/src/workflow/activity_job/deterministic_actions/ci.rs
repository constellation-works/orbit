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
