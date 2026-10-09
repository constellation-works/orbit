//! Pull ordering through the owner's serialized admission boundary.

use orbit_store::contracts::TaskDocumentUpdateParams;
use orbit_types::task::{HostOs, Task};

use super::*;

fn candidate(owner: &Coordinated, title: &str, priority: TaskPriority, tags: &[&str]) -> Task {
    let task = owner.create_task(title);
    owner
        .backends
        .task
        .document
        .update_task_document(
            &task.id,
            TaskDocumentUpdateParams {
                actor: "codex".into(),
                priority: Some(priority),
                task_type: Some(TaskType::Bug),
                tags: Some(tags.iter().map(ToString::to_string).collect()),
                ..Default::default()
            },
        )
        .unwrap();
    task
}

/// A scarce-OS requester takes work the owner cannot run, without crossing
/// priority or expiry ordering. Shared OS requirements and local admission
/// retain age order; an undeclared requester still cannot take tagged work.
#[test]
fn affinity_respects_priority_expiry_and_os_eligibility() {
    if !isolated("ordering::affinity_respects_priority_expiry_and_os_eligibility") {
        return;
    }
    use HostOs::{Linux, Macos};
    use TaskPriority::{Critical, High, Medium};
    struct Case {
        owner: Option<HostOs>,
        requester: Option<HostOs>,
        priorities: [TaskPriority; 2],
        tags: &'static [&'static str],
        expiring: [bool; 2],
        prefer_newer: bool,
    }
    let ordinary = Case {
        owner: Some(Linux),
        requester: Some(Macos),
        priorities: [High, High],
        tags: &["os:macos"],
        expiring: [false, false],
        prefer_newer: true,
    };
    let cases = [
        Case { ..ordinary },
        Case {
            priorities: [Critical, High],
            prefer_newer: false,
            ..ordinary
        },
        Case {
            priorities: [High, Medium],
            prefer_newer: false,
            ..ordinary
        },
        Case {
            tags: &["os:linux", "os:macos"],
            prefer_newer: false,
            ..ordinary
        },
        Case {
            owner: Some(Macos),
            prefer_newer: false,
            ..ordinary
        },
        Case {
            owner: None,
            ..ordinary
        },
        Case {
            requester: None,
            prefer_newer: false,
            ..ordinary
        },
        Case {
            expiring: [true, false],
            prefer_newer: false,
            ..ordinary
        },
        Case {
            expiring: [true, true],
            ..ordinary
        },
    ];
    for (index, case) in cases.into_iter().enumerate() {
        let root = TempDir::new().unwrap();
        let owner = Coordinated::open(root.path());
        let older = candidate(&owner, "older unrestricted bug", case.priorities[0], &[]);
        let newer = candidate(
            &owner,
            "newer OS-specific bug",
            case.priorities[1],
            case.tags,
        );
        assert!(older.created_at < newer.created_at);
        let tasks = [&older, &newer];
        let ordering = AdmissionOrdering {
            owner_os: case.owner,
            expiring_tasks: tasks
                .iter()
                .zip(case.expiring)
                .filter(|(_, expiring)| *expiring)
                .map(|(task, _)| task.id.clone())
                .collect(),
        };
        let mut request = owner_request("order");
        request.os = case.requester;
        let AdmissionLookup::Found { receipt, .. } =
            owner.try_pull_with_ordering(&request, &ordering).unwrap()
        else {
            panic!("case {index}: no receipt");
        };
        let (selected, waiting) = if case.prefer_newer {
            (&newer, &older)
        } else {
            (&older, &newer)
        };
        assert_eq!(receipt.claim.unwrap().task_id, selected.id, "case {index}");
        assert_eq!(owner.task_status(&selected.id), TaskStatus::InProgress);
        assert_eq!(owner.task_status(&waiting.id), TaskStatus::Backlog);
        // A retry keeps its first decision even when owner ordering changes.
        let mut reordered = ordering;
        reordered.expiring_tasks = [waiting.id.clone()].into();
        let AdmissionLookup::Found { receipt, .. } =
            owner.try_pull_with_ordering(&request, &reordered).unwrap()
        else {
            panic!("case {index}: no replay receipt");
        };
        assert_eq!(receipt.claim.unwrap().task_id, selected.id, "case {index}");
    }
}
