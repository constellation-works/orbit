//! [ORB-22649] A step's `reuse_when` settles it on its definition's accepted
//! result, revalidated for the execution, instead of minting. A result for
//! another revision or past the contract's age mints fresh evidence, missing
//! history mints, unreadable history holds, and a reused result is still
//! revalidated before a later step mints.

use std::cell::Cell;
use std::rc::Rc;

use chrono::{Duration, Utc};
use orbit_core::OrbitRuntime;
use orbit_core::application::task::TaskUpdateParams;
use orbit_engine::{AutoTaskAdmission, RuntimeHost};
use orbit_types::task::TaskStatus;
use orbit_types::workflow::AutoTaskStepStatus;
use serde_json::{Value, json};

use super::shared_completion::{
    ContractFixture, TARGET, admit_run, complete_with_evidence, observe_step, reserve_step,
};

const PREP_TARGET: &str = "prepare-release";

/// The fixture's `qa` step, reusing any passed result. Revision and age are
/// left to the owner's revalidation, which the predicate cannot relax.
const REUSE_QA: &str = "    - id: qa\n      target: auto-task:full-qa-sweep\n      require_result: release-readiness/v1\n      reuse_when: \"result.status == 'passed'\"\n";
const PREP: &str = "    - id: prep\n      target: auto-task:prepare-release\n";

fn admit(runtime: &OrbitRuntime, run: &str, step: &str) -> AutoTaskAdmission {
    RuntimeHost::admit_auto_task_step(runtime, run, step).unwrap()
}

fn history_path(runtime: &OrbitRuntime) -> std::path::PathBuf {
    runtime
        .paths()
        .state_dir
        .join("auto-task-accepted-results.json")
}

fn archive(runtime: &OrbitRuntime, task: &str) {
    runtime
        .force_update_task_with_identity(
            task,
            TaskUpdateParams {
                status: Some(TaskStatus::Archived),
                ..Default::default()
            },
            None,
            None,
        )
        .unwrap();
}

fn reuse_when_settles_on_a_revalidated_receipt_and_otherwise_mints_or_holds() {
    let fixture = ContractFixture::new();
    let blocked = Rc::new(Cell::new(false));
    let adapter = fixture.adapter(Rc::clone(&blocked));
    let runtime = &fixture.runtime;
    let revision = &fixture.revision;
    super::add_auto_task(runtime, PREP_TARGET);
    orbit_core::test_support::with_completion_adapter_run(adapter, || {
        // No accepted result yet: nothing to reuse, so the step mints.
        let first = admit_run(runtime, "first", revision, REUSE_QA);
        let mut record = reserve_step(runtime, &first, "qa", TARGET);
        let AutoTaskAdmission::Bound {
            task_id: child,
            minted: true,
        } = admit(runtime, &first, "qa")
        else {
            panic!("missing history must mint")
        };
        record.task_id = Some(child.clone());
        record.minted = true;
        record.status = AutoTaskStepStatus::Queued;
        RuntimeHost::checkpoint_auto_task_step(runtime, &first, "qa", &record).unwrap();
        complete_with_evidence(runtime, &child, &first, revision);
        let receipt = observe_step(runtime, &first, "qa", TARGET).receipt.unwrap();
        assert!(receipt.result.is_some(), "{receipt:?}");
        record.receipt = Some(receipt);
        record.status = AutoTaskStepStatus::Done;
        record.settled_at = Some(Utc::now());
        RuntimeHost::checkpoint_auto_task_step(runtime, &first, "qa", &record).unwrap();
        let accepted: Value =
            serde_json::from_str(&std::fs::read_to_string(history_path(runtime)).unwrap()).unwrap();

        // The pinned revision's accepted result settles the step: no mint.
        let reuse = admit_run(runtime, "reuse", revision, &format!("{REUSE_QA}{PREP}"));
        let mut record = reserve_step(runtime, &reuse, "qa", TARGET);
        let AutoTaskAdmission::Reused(receipt) = admit(runtime, &reuse, "qa") else {
            panic!("a valid accepted result for the pinned revision must be reused")
        };
        assert_eq!(receipt.task_id, child);
        assert!(receipt.accepted);
        let result = receipt.result.as_ref().unwrap();
        assert_eq!(&result.revision, revision);
        assert_eq!(runtime.list_tasks().unwrap().len(), 1, "reuse never mints");
        record.task_id = Some(child.clone());
        record.receipt = Some(*receipt);
        record.status = AutoTaskStepStatus::Done;
        record.settled_at = Some(Utc::now());
        RuntimeHost::checkpoint_auto_task_step(runtime, &reuse, "qa", &record).unwrap();

        // A later step's mint asks the adapter about the reused result again.
        reserve_step(runtime, &reuse, "prep", PREP_TARGET);
        blocked.set(true);
        let AutoTaskAdmission::Held(hold) = admit(runtime, &reuse, "prep") else {
            panic!("a reused result the adapter no longer passes must hold the later step")
        };
        assert!(hold.contains("step `qa`"), "{hold}");
        assert_eq!(runtime.list_tasks().unwrap().len(), 1);
        blocked.set(false);
        assert!(matches!(
            admit(runtime, &reuse, "prep"),
            AutoTaskAdmission::Bound { minted: true, .. }
        ));
        let tasks = runtime.list_tasks().unwrap().len();

        // Unreadable history holds rather than minting.
        std::fs::write(history_path(runtime), "{not json").unwrap();
        let unreadable = admit_run(runtime, "unreadable", revision, REUSE_QA);
        reserve_step(runtime, &unreadable, "qa", TARGET);
        let AutoTaskAdmission::Held(hold) = admit(runtime, &unreadable, "qa") else {
            panic!("unreadable history must hold")
        };
        assert!(hold.contains("unreadable"), "{hold}");
        assert_eq!(runtime.list_tasks().unwrap().len(), tasks);

        // Another revision's result, or one past the contract's age, does
        // not match even though the predicate does: the step mints fresh.
        let stale = [
            ("other-revision", "revision", json!("0".repeat(40))),
            (
                "expired",
                "completed_at",
                json!(Utc::now() - Duration::hours(2)),
            ),
        ];
        for (name, field, value) in stale {
            let mut history = accepted.clone();
            history["definitions"][TARGET][field] = value;
            std::fs::write(history_path(runtime), history.to_string()).unwrap();
            let run = admit_run(runtime, name, revision, REUSE_QA);
            reserve_step(runtime, &run, "qa", TARGET);
            let AutoTaskAdmission::Bound {
                task_id,
                minted: true,
            } = admit(runtime, &run, "qa")
            else {
                panic!("{name}: a result that does not apply must mint fresh evidence")
            };
            assert_ne!(task_id, child, "{name}");
            archive(runtime, &task_id);
        }
    });
}

crate::scenario_table!(
    reuse_scenarios,
    reuse_when_settles_on_a_revalidated_receipt_and_otherwise_mints_or_holds
);
