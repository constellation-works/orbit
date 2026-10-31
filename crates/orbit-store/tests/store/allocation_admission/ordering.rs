//! Pull ordering through the owner's serialized admission boundary, against
//! the live executor census [ORB-17304].
//!
//! The executors are a Mac that alone runs the `opus` crew, `dk-server-1`
//! (Linux, the only verified browser kit) and the Linux owner. Each case
//! pulls once from a fresh store and names the task its requester takes.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{Duration as ChronoDuration, Utc};
use orbit_store::contracts::{
    AdmissionAffinityClass, AdmissionCrewCapability, AdmissionCrewPressure,
    EXECUTOR_CENSUS_LIVENESS_SECONDS, ExecutorCapability, ExecutorCensus, ExecutorCensusEntry,
    TaskDocumentUpdateParams,
};
use orbit_types::task::{AdmissionAffinityReach, ExecutorOsLimit, HostOs, Task, TaskComplexity};

use super::*;

const MAC: &str = "mac";
const DK: &str = "dk-server-1";
const OWNER: &str = "owner";

/// One backlog candidate: title, priority, type, tags and crew.
type Candidate = (
    &'static str,
    TaskPriority,
    TaskType,
    &'static [&'static str],
    Option<&'static str>,
);

fn candidate(owner: &Coordinated, index: usize, spec: &Candidate) -> Task {
    let (title, priority, task_type, tags, crew) = *spec;
    let selector = format!("src/candidate_{index}.rs");
    let task = owner.create_task_in(title, &[selector.as_str()]);
    owner
        .backends
        .task
        .document
        .update_task_document(
            &task.id,
            TaskDocumentUpdateParams {
                actor: "codex".into(),
                priority: Some(priority),
                task_type: Some(task_type),
                tags: Some(tags.iter().map(ToString::to_string).collect()),
                crew: Some(crew.map(ToString::to_string)),
                ..Default::default()
            },
        )
        .unwrap();
    task
}

fn crews(runnable: &[&str]) -> AdmissionCrewCapability {
    AdmissionCrewCapability {
        runnable: Some(runnable.iter().map(ToString::to_string).collect()),
        default_crew: Some("codex".into()),
        excluded: Vec::new(),
        usage_pressure: Vec::new(),
    }
}

/// What each executor of the mix declares.
fn capability(machine: &str) -> ExecutorCapability {
    match machine {
        MAC => ExecutorCapability {
            labels: Vec::new(),
            os: Some(HostOs::Macos),
            crews: Some(crews(&["codex", "opus", "xcode"])),
            browser_kit: false,
        },
        DK => ExecutorCapability {
            labels: Vec::new(),
            os: Some(HostOs::Linux),
            crews: Some(crews(&["codex", "xcode"])),
            browser_kit: true,
        },
        _ => ExecutorCapability {
            labels: Vec::new(),
            os: Some(HostOs::Linux),
            crews: Some(crews(&["codex", "xcode"])),
            browser_kit: false,
        },
    }
}

fn entry(machine: &str, seen_at: chrono::DateTime<Utc>) -> ExecutorCensusEntry {
    ExecutorCensusEntry {
        machine_id: machine.into(),
        capability: capability(machine),
        run_id: format!("{machine}-drain"),
        seen_at,
    }
}

fn request_from(machine: &str, id: &str) -> AdmissionRequest {
    let declared = capability(machine);
    let mut request = owner_request(id);
    request.os = declared.os;
    request.crews = declared.crews;
    request.browser_kit = declared.browser_kit;
    request
}

/// `xcode`'s executor runs only on macOS: a crew-derived `os:macos`.
fn host_os_limits() -> BTreeMap<String, ExecutorOsLimit> {
    [(
        "xcode".to_string(),
        ExecutorOsLimit {
            executor: "xcode".into(),
            hosts: BTreeSet::from([HostOs::Macos]),
        },
    )]
    .into()
}

/// Admission order per requester, census and `workflow.admission_affinity`
/// reach. Critical and corrective work leads for every executor; within a
/// band the requester takes what only it can run, then what fewer live
/// executors can run. A census of the requester alone, or none, keeps the
/// canonical order, and a retry keeps its first decision.
#[test]
fn census_orders_exclusive_work_first_within_a_band() {
    if !allocation_admission_isolated("ordering::census_orders_exclusive_work_first_within_a_band")
    {
        return;
    }
    use AdmissionAffinityClass::{Exclusive, Fungible, Shared};
    use AdmissionAffinityReach::{Band, Off, Priority};
    use TaskPriority::{Critical, High, Medium};
    use TaskType::{Bug, Feature};
    const FUNGIBLE_HIGH: Candidate = ("fungible high", High, Feature, &[], None);
    const OPUS_MEDIUM: Candidate = ("opus medium", Medium, Feature, &[], Some("opus"));
    const OPUS_HIGH: Candidate = ("opus high", High, Feature, &[], Some("opus"));
    const FULL: &[&str] = &[MAC, DK, OWNER];
    struct Case {
        name: &'static str,
        requester: &'static str,
        census: Option<&'static [&'static str]>,
        reach: AdmissionAffinityReach,
        candidates: &'static [Candidate],
        expiring: &'static [&'static str],
        takes: &'static str,
        class: Option<(AdmissionAffinityClass, usize)>,
    }
    let cases = [
        Case {
            name: "the Mac takes a medium opus task over a high fungible one",
            requester: MAC,
            census: Some(FULL),
            reach: Band,
            candidates: &[FUNGIBLE_HIGH, OPUS_MEDIUM],
            expiring: &[],
            takes: "opus medium",
            class: Some((Exclusive, 1)),
        },
        Case {
            name: "dk-server-1 takes the fungible task",
            requester: DK,
            census: Some(FULL),
            reach: Band,
            candidates: &[FUNGIBLE_HIGH, OPUS_MEDIUM],
            expiring: &[],
            takes: "fungible high",
            class: Some((Fungible, 3)),
        },
        Case {
            name: "an os:macos tag is the Mac's alone",
            requester: MAC,
            census: Some(FULL),
            reach: Band,
            candidates: &[
                FUNGIBLE_HIGH,
                ("macos medium", Medium, Feature, &["os:macos"], None),
            ],
            expiring: &[],
            takes: "macos medium",
            class: Some((Exclusive, 1)),
        },
        Case {
            name: "a crew whose executor runs only on macOS is the Mac's alone",
            requester: MAC,
            census: Some(FULL),
            reach: Band,
            candidates: &[
                FUNGIBLE_HIGH,
                ("xcode medium", Medium, Feature, &[], Some("xcode")),
            ],
            expiring: &[],
            takes: "xcode medium",
            class: Some((Exclusive, 1)),
        },
        Case {
            name: "critical work leads the Mac's exclusive work",
            requester: MAC,
            census: Some(FULL),
            reach: Band,
            candidates: &[
                ("fungible critical", Critical, Feature, &[], None),
                OPUS_HIGH,
            ],
            expiring: &[],
            takes: "fungible critical",
            class: Some((Fungible, 3)),
        },
        Case {
            name: "corrective work leads the Mac's exclusive work",
            requester: MAC,
            census: Some(FULL),
            reach: Band,
            candidates: &[("fungible bug", Medium, Bug, &[], None), OPUS_HIGH],
            expiring: &[],
            takes: "fungible bug",
            class: Some((Fungible, 3)),
        },
        Case {
            name: "an expiring batch joins the corrective band ahead of exclusive work",
            requester: MAC,
            census: Some(FULL),
            reach: Band,
            candidates: &[FUNGIBLE_HIGH, OPUS_HIGH],
            expiring: &["fungible high"],
            takes: "fungible high",
            class: Some((Fungible, 3)),
        },
        Case {
            name: "priority reach keeps a higher priority ahead",
            requester: MAC,
            census: Some(FULL),
            reach: Priority,
            candidates: &[FUNGIBLE_HIGH, OPUS_MEDIUM],
            expiring: &[],
            takes: "fungible high",
            class: Some((Fungible, 3)),
        },
        Case {
            name: "priority reach puts exclusive work first within a priority",
            requester: MAC,
            census: Some(FULL),
            reach: Priority,
            candidates: &[FUNGIBLE_HIGH, OPUS_HIGH],
            expiring: &[],
            takes: "opus high",
            class: Some((Exclusive, 1)),
        },
        Case {
            name: "off keeps age order",
            requester: MAC,
            census: Some(FULL),
            reach: Off,
            candidates: &[FUNGIBLE_HIGH, OPUS_HIGH],
            expiring: &[],
            takes: "fungible high",
            class: Some((Fungible, 3)),
        },
        Case {
            name: "a census of the requester alone keeps age order",
            requester: MAC,
            census: Some(&[MAC]),
            reach: Band,
            candidates: &[FUNGIBLE_HIGH, OPUS_HIGH],
            expiring: &[],
            takes: "fungible high",
            class: Some((Fungible, 1)),
        },
        Case {
            name: "no census keeps age order",
            requester: MAC,
            census: None,
            reach: Band,
            candidates: &[FUNGIBLE_HIGH, OPUS_HIGH],
            expiring: &[],
            takes: "fungible high",
            class: None,
        },
        Case {
            name: "dk-server-1 takes work fewer executors can run",
            requester: DK,
            census: Some(FULL),
            reach: Band,
            candidates: &[
                FUNGIBLE_HIGH,
                ("linux high", High, Feature, &["os:linux"], None),
            ],
            expiring: &[],
            takes: "linux high",
            class: Some((Shared, 2)),
        },
        Case {
            name: "dk-server-1's browser kit makes ui work its own",
            requester: DK,
            census: Some(FULL),
            reach: Band,
            candidates: &[FUNGIBLE_HIGH, ("ui medium", Medium, Feature, &["ui"], None)],
            expiring: &[],
            takes: "ui medium",
            class: Some((Exclusive, 1)),
        },
    ];
    for case in cases {
        let root = TempDir::new().unwrap();
        let owner = Coordinated::open(root.path());
        let tasks = case
            .candidates
            .iter()
            .enumerate()
            .map(|(index, spec)| candidate(&owner, index, spec))
            .collect::<Vec<_>>();
        let id_of = |title: &str| {
            tasks
                .iter()
                .find(|task| task.title == title)
                .map(|task| task.id.clone())
                .unwrap()
        };
        let request = request_from(case.requester, "order");
        let now = Utc::now();
        let ordering = AdmissionOrdering {
            affinity: case.reach,
            census: case.census.map(|members| {
                ExecutorCensus::new(
                    case.requester,
                    ExecutorCapability::of_request(&request),
                    members.iter().map(|machine| entry(machine, now)),
                )
            }),
            expiring_tasks: case.expiring.iter().map(|title| id_of(title)).collect(),
            executor_host_os: host_os_limits(),
            ..Default::default()
        };
        let AdmissionLookup::Found { receipt, .. } = owner
            .try_pull_as(case.requester, &request, &ordering)
            .unwrap()
        else {
            panic!("{}: no receipt", case.name);
        };
        let taken = id_of(case.takes);
        assert_eq!(
            receipt.claim.as_ref().map(|claim| &claim.task_id),
            Some(&taken),
            "{}",
            case.name
        );
        let recorded = receipt
            .affinity
            .as_ref()
            .map(|affinity| (affinity.class, affinity.eligible_executors));
        assert_eq!(recorded, case.class, "{}: receipt affinity", case.name);
        if let (Some(affinity), Some(members)) = (&receipt.affinity, case.census) {
            let mut census = members.iter().map(ToString::to_string).collect::<Vec<_>>();
            census.sort();
            assert_eq!(affinity.census, census, "{}: receipt census", case.name);
            assert_eq!(affinity.reach, case.reach, "{}: receipt reach", case.name);
        }
        for task in &tasks {
            let expected = if task.id == taken {
                TaskStatus::InProgress
            } else {
                TaskStatus::Backlog
            };
            assert_eq!(owner.task_status(&task.id), expected, "{}", case.name);
        }
        // A retry keeps its first decision even when the census is gone.
        let AdmissionLookup::Found {
            receipt: replay, ..
        } = owner
            .try_pull_as(case.requester, &request, &AdmissionOrdering::default())
            .unwrap()
        else {
            panic!("{}: no replay receipt", case.name);
        };
        assert_eq!(replay, receipt, "{}: replay", case.name);
    }
}

/// Each pull upserts its requester's census row with its receipt, idle or
/// admitted. A row stays live within the liveness window, and past it while
/// its machine holds a claimed claim; an older declaration never replaces a
/// newer one.
#[test]
fn census_rows_land_with_receipts_and_stay_live_while_claiming() {
    if !allocation_admission_isolated(
        "ordering::census_rows_land_with_receipts_and_stay_live_while_claiming",
    ) {
        return;
    }
    let root = TempDir::new().unwrap();
    let owner = Coordinated::open(root.path());
    let boundary = &owner.backends.commit_boundary;
    let live = |at| {
        boundary
            .live_executor_census(at)
            .unwrap()
            .into_iter()
            .map(|entry| (entry.machine_id.clone(), entry))
            .collect::<BTreeMap<_, _>>()
    };
    owner.create_task("the only task");
    let ordering = AdmissionOrdering::default();
    let AdmissionLookup::Found { receipt, .. } = owner
        .try_pull_as(MAC, &request_from(MAC, "mac-pull"), &ordering)
        .unwrap()
    else {
        panic!("no receipt");
    };
    assert!(receipt.claim.is_some(), "the Mac admits the task");
    let AdmissionLookup::Found { receipt, .. } = owner
        .try_pull_as(DK, &request_from(DK, "dk-pull"), &ordering)
        .unwrap()
    else {
        panic!("no idle receipt");
    };
    assert!(receipt.claim.is_none(), "nothing is left for dk-server-1");

    let now = Utc::now();
    let rows = live(now);
    assert_eq!(rows.keys().collect::<Vec<_>>(), [DK, MAC]);
    assert_eq!(rows[MAC].capability, capability(MAC));
    assert_eq!(rows[MAC].run_id, "drain");
    assert_eq!(rows[DK].capability, capability(DK));
    let later = now + ChronoDuration::seconds(EXECUTOR_CENSUS_LIVENESS_SECONDS + 60);
    assert_eq!(
        live(later).keys().collect::<Vec<_>>(),
        [MAC],
        "past the window only the claiming Mac stays live"
    );

    boundary
        .record_executor_census(&entry(OWNER, now - ChronoDuration::hours(1)))
        .unwrap();
    assert!(
        !live(now).contains_key(OWNER),
        "a stale owner row is not live"
    );
    boundary.record_executor_census(&entry(OWNER, now)).unwrap();
    assert!(live(now).contains_key(OWNER), "a fresh owner row is live");
    let mut older = entry(OWNER, now - ChronoDuration::minutes(1));
    older.run_id = "an older drain".into();
    boundary.record_executor_census(&older).unwrap();
    assert_eq!(
        live(now)[OWNER].run_id,
        "owner-drain",
        "older never replaces newer"
    );
}

/// [ORB-17773] The requester's reported weekly pressure is the lowest
/// ordering key above age: of two tasks tied on band, priority, expiry
/// and census reach, the one whose pool contains the requester's
/// highest-pressure crew is offered first. A higher priority and an
/// expiring task still lead, and a requester reporting no pressure
/// keeps age order exactly. The admitted receipt carries the task's
/// automatic crew source for the follower's own re-draw.
#[test]
fn pressure_orders_tied_candidates_below_priority_and_expiry() {
    if !allocation_admission_isolated(
        "ordering::pressure_orders_tied_candidates_below_priority_and_expiry",
    ) {
        return;
    }
    use TaskPriority::{High, Medium};

    fn pooled(
        owner: &Coordinated,
        index: usize,
        title: &str,
        priority: TaskPriority,
        complexity: TaskComplexity,
        source: &str,
    ) -> Task {
        let selector = format!("src/pressure_{index}.rs");
        let task = owner.create_task_in(title, &[selector.as_str()]);
        owner
            .backends
            .task
            .document
            .update_task_document(
                &task.id,
                TaskDocumentUpdateParams {
                    actor: "codex".into(),
                    priority: Some(priority),
                    complexity: Some(complexity),
                    crew: Some(Some("codex".to_string())),
                    crew_source: Some(Some(source.to_string())),
                    ..Default::default()
                },
            )
            .unwrap();
        task
    }

    fn pools() -> BTreeMap<String, Vec<String>> {
        BTreeMap::from([
            ("low".to_string(), vec!["codex".to_string()]),
            (
                "hard".to_string(),
                vec!["opus".to_string(), "codex".to_string()],
            ),
        ])
    }

    // (name, older priority, pressure reported, older expiring, takes older)
    let cases = [
        (
            "tied: the pressured pool leads age",
            Medium,
            true,
            false,
            false,
        ),
        (
            "no pressure reported: age order",
            Medium,
            false,
            false,
            true,
        ),
        ("higher priority still leads", High, true, false, true),
        ("an expiring task still leads", Medium, true, true, true),
    ];
    for (name, older_priority, reported, expiring, takes_older) in cases {
        let root = TempDir::new().unwrap();
        let owner = Coordinated::open(root.path());
        let older = pooled(
            &owner,
            0,
            "older plain",
            older_priority,
            TaskComplexity::Low,
            "pool:low",
        );
        let newer = pooled(
            &owner,
            1,
            "newer pressured",
            Medium,
            TaskComplexity::Hard,
            "pool:hard",
        );
        let mut request = request_from(DK, "pressure");
        if reported {
            request.crews.as_mut().unwrap().usage_pressure = vec![
                AdmissionCrewPressure {
                    crew: "opus".into(),
                    pressure_milli: 25_000,
                },
                AdmissionCrewPressure {
                    crew: "codex".into(),
                    pressure_milli: 1_000,
                },
            ];
        }
        let ordering = AdmissionOrdering {
            affinity: AdmissionAffinityReach::Band,
            census: Some(ExecutorCensus::new(
                DK,
                ExecutorCapability::of_request(&request),
                Vec::new(),
            )),
            expiring_tasks: expiring.then(|| older.id.clone()).into_iter().collect(),
            crew_pools: pools(),
            ..Default::default()
        };
        let AdmissionLookup::Found { receipt, .. } =
            owner.try_pull_as(DK, &request, &ordering).unwrap()
        else {
            panic!("{name}: no receipt");
        };
        let expected = if takes_older { &older } else { &newer };
        assert_eq!(
            receipt.claim.as_ref().map(|claim| &claim.task_id),
            Some(&expected.id),
            "{name}"
        );
        let summary = receipt.task.as_ref().expect("task summary");
        assert_eq!(summary.id, expected.id, "{name}");
        assert_eq!(
            summary.crew_source.as_deref(),
            Some(if takes_older { "pool:low" } else { "pool:hard" }),
            "{name}: the receipt carries the automatic source"
        );
        assert_eq!(
            summary.complexity,
            Some(if takes_older {
                TaskComplexity::Low
            } else {
                TaskComplexity::Hard
            }),
            "{name}"
        );
    }
}
