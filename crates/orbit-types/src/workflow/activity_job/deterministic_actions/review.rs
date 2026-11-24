macro_rules! review_actions {
    ($collect:ident, $state:tt) => {
        $collect! {
            $state;
            core {
                FulfilReviewEvidence = 12 => "fulfil_review_evidence",
                RefreshBaselineHolds = 26 => "refresh_baseline_holds",
                ReviewGateAdmit = 33 => "review_gate_admit",
                ReviewGateSettle = 34 => "review_gate_settle",
                ReviewReconciliationPrepare = 35 => "review_reconciliation_prepare",
                ReviewReconciliationSettle = 36 => "review_reconciliation_settle",
                ReviewReconciliationValidate = 37 => "review_reconciliation_validate",
            }
            engine {}
        }
    };
}

pub(super) use review_actions;
