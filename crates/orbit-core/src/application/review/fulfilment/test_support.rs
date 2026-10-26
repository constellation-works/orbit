//! Completed external analysis and deterministic findings-transition interleavings.

use std::cell::RefCell;

use orbit_types::workflow::{ReviewEvidenceFinding, ReviewEvidenceHold};

use super::{EvidenceRun, FulfilmentRefusal};

struct Completion {
    findings: Vec<ReviewEvidenceFinding>,
    before_transition: Option<Box<dyn FnOnce()>>,
}

thread_local! {
    static COMPLETION: RefCell<Option<Completion>> = const { RefCell::new(None) };
}

/// Supply completed external findings, then interleave a task update just before
/// the findings transition enters its critical section. This exercises the real
/// public fulfilment action and artifact writes without running an analyzer.
/// Scoped to this thread; restores the previous fixture on return or unwind.
pub fn with_review_evidence_completion<T>(
    findings: Vec<ReviewEvidenceFinding>,
    before_transition: impl FnOnce() + 'static,
    operation: impl FnOnce() -> T,
) -> T {
    struct Restore(Option<Completion>);
    impl Drop for Restore {
        fn drop(&mut self) {
            COMPLETION.with(|slot| slot.replace(self.0.take()));
        }
    }
    let _restore = Restore(COMPLETION.with(|slot| {
        slot.replace(Some(Completion {
            findings,
            before_transition: Some(Box::new(before_transition)),
        }))
    }));
    operation()
}

pub(super) fn completed_findings(hold: &ReviewEvidenceHold) -> Option<Vec<EvidenceRun>> {
    COMPLETION.with(|slot| {
        let completion = slot.borrow();
        let completion = completion.as_ref()?;
        Some(
            hold.requirements
                .iter()
                .map(|requirement| EvidenceRun {
                    exit_code: Some(0),
                    refusal: Some(FulfilmentRefusal::FindingsReported),
                    detail: "completed analysis reported findings".into(),
                    findings: completion.findings.clone(),
                    ..EvidenceRun::new(requirement)
                })
                .collect(),
        )
    })
}

pub(crate) fn before_findings_transition() {
    let hook = COMPLETION.with(|slot| {
        slot.borrow_mut()
            .as_mut()
            .and_then(|completion| completion.before_transition.take())
    });
    if let Some(hook) = hook {
        hook();
    }
}
