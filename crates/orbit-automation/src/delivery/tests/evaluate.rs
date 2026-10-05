use super::evidence::{now, revision, setup};
use crate::delivery::{self, Evaluation};

use std::sync::Arc;

#[test]
fn concurrent_evaluators_admit_one_action() {
    let (store, host, trigger) = setup();
    host.page(0, 2);
    let host = Arc::new(host);
    std::thread::scope(|scope| {
        let handles = (0..8)
            .map(|_| {
                let store = store.clone();
                let host = host.clone();
                let trigger = &trigger;
                scope.spawn(move || {
                    delivery::evaluate(
                        store.as_ref(),
                        host.as_ref(),
                        Evaluation {
                            consumer: "ws/qa",
                            epoch: "v1",
                            trigger,
                            enabled: true,
                            dry_run: false,
                            now: now(),
                        },
                    )
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            let _ = handle.join().unwrap();
        }
    });
    assert_eq!(host.actions.lock().unwrap().len(), 1);
    let state = store.automation_state("ws/qa").unwrap().unwrap();
    assert_eq!(state.pending.len(), 2);
    assert_eq!(state.covered, revision(0));
}
