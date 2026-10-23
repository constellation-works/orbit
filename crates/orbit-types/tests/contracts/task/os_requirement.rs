//! Combinatorial `os:` tag semantics: every host against every tag shape.
//! An executor `host_os` narrows that parse; omitting it leaves the tag
//! wording unchanged.

use std::collections::{BTreeMap, BTreeSet};

use orbit_types::task::{ExecutorOsLimit, HostOs, TaskOsRequirement, validate_os_tags};

fn tags(values: &[&str]) -> Vec<String> {
    values.iter().map(ToString::to_string).collect()
}

#[test]
fn each_tag_shape_admits_exactly_the_hosts_it_names() {
    let hosts = [
        None,
        Some(HostOs::Linux),
        Some(HostOs::Macos),
        Some(HostOs::Windows),
    ];
    // (tags, hosts that may start the task), in `hosts` order.
    let cases: [(&[&str], [bool; 4]); 6] = [
        (&[], [true, true, true, true]),
        (&["bug", "os:macos"], [false, false, true, false]),
        (&["OS:Linux"], [false, true, false, false]),
        (&["os:linux", "os:macos"], [false, true, true, false]),
        (&["os:windows"], [false, false, false, true]),
        // A stored tag outside the namespace (written before it was
        // reserved) is satisfied by no host, alongside a valid one too.
        (&["os:mac", "os:macos"], [false, false, false, false]),
    ];
    for (task_tags, expected) in cases {
        let requirement = TaskOsRequirement::from_tags(&tags(task_tags));
        for (host, admits) in hosts.into_iter().zip(expected) {
            assert_eq!(
                requirement.satisfied_by(host),
                admits,
                "{task_tags:?} on {host:?}"
            );
            assert_eq!(
                requirement.unsatisfied_reason(host).is_none(),
                admits,
                "{task_tags:?} on {host:?}"
            );
        }
    }
}

#[test]
fn only_the_reserved_values_pass_a_tag_write() {
    for valid in [
        &[][..],
        &["os:linux"],
        &["OS:MacOS", "os:windows"],
        &["osx"],
    ] {
        assert!(validate_os_tags(&tags(valid)).is_ok(), "{valid:?}");
    }
    for invalid in [&["os:mac"][..], &["os:"], &["os:linux", "Os:Darwin"]] {
        assert!(validate_os_tags(&tags(invalid)).is_err(), "{invalid:?}");
    }
}

fn macos_only() -> BTreeSet<HostOs> {
    BTreeSet::from([HostOs::Macos])
}

/// Omitting `host_os` returns the tag requirement, description included.
#[test]
fn an_omitted_executor_limit_keeps_the_tag_wording() {
    let requirement = TaskOsRequirement::from_tags(&tags(&["os:macos"]));
    let same = requirement
        .intersect_executor("muse-wonder", "muse", None)
        .expect("omission is unrestricted");
    assert_eq!(same, requirement);
    assert_eq!(
        same.unsatisfied_reason(Some(HostOs::Linux)).as_deref(),
        Some("waits for a macos host (os:macos)")
    );
}

/// An untagged task assigned a constrained crew waits for that crew's hosts.
#[test]
fn an_untagged_task_takes_the_executor_hosts_and_names_the_crew() {
    let hosts = macos_only();
    let narrowed = TaskOsRequirement::from_tags(&tags(&[]))
        .intersect_executor("muse-wonder", "muse", Some(&hosts))
        .expect("the executor's hosts are a requirement");
    assert_eq!(
        narrowed.unsatisfied_reason(Some(HostOs::Linux)).as_deref(),
        Some("waits for a macos host (crew muse-wonder)")
    );
    assert!(narrowed.satisfied_by(Some(HostOs::Macos)));
    assert!(!narrowed.satisfied_by(None));
    assert!(!narrowed.satisfied_by(Some(HostOs::Windows)));
}

/// Tags and `host_os` keep only the hosts in both, still naming the crew.
#[test]
fn a_partial_intersection_keeps_the_shared_hosts_and_the_crew() {
    let hosts = macos_only();
    let narrowed = TaskOsRequirement::from_tags(&tags(&["os:linux", "os:macos"]))
        .intersect_executor("muse-wonder", "muse", Some(&hosts))
        .expect("macos is in both");
    assert!(narrowed.satisfied_by(Some(HostOs::Macos)));
    assert!(!narrowed.satisfied_by(Some(HostOs::Linux)));
    assert_eq!(
        narrowed.unsatisfied_reason(Some(HostOs::Linux)).as_deref(),
        Some("waits for a macos host (crew muse-wonder)")
    );
}

/// An empty intersection names the crew, the executor constraint and the tags.
#[test]
fn an_empty_intersection_names_the_crew_the_constraint_and_the_tags() {
    let hosts = macos_only();
    let error = TaskOsRequirement::from_tags(&tags(&["OS:Linux"]))
        .intersect_executor("muse-wonder", "muse", Some(&hosts))
        .expect_err("linux and macos do not overlap");
    assert_eq!(
        error,
        "crew `muse-wonder` (executor `muse`, host_os: macos) does not intersect the task's \
         tags `os:linux`; no host can run it"
    );
}

/// `for_crew` applies only the assigned crew's entry. Other crews stay on tags.
#[test]
fn for_crew_applies_only_the_assigned_crews_limit() {
    let limits = BTreeMap::from([(
        "muse-wonder".to_string(),
        ExecutorOsLimit {
            executor: "muse".to_string(),
            hosts: macos_only(),
        },
    )]);
    let muse = TaskOsRequirement::for_crew(&tags(&[]), Some("Muse-Wonder"), &limits)
        .expect("crew lookup ignores case");
    assert_eq!(
        muse.unsatisfied_reason(Some(HostOs::Linux)).as_deref(),
        Some("waits for a macos host (crew Muse-Wonder)")
    );
    let sol = TaskOsRequirement::for_crew(&tags(&["os:macos"]), Some("sol"), &limits)
        .expect("a crew with no limit is tags alone");
    assert_eq!(
        sol.unsatisfied_reason(Some(HostOs::Linux)).as_deref(),
        Some("waits for a macos host (os:macos)")
    );
    assert!(
        TaskOsRequirement::for_crew(&tags(&[]), None, &limits)
            .expect("no crew")
            .is_unrestricted()
    );
}
