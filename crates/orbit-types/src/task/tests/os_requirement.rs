//! Combinatorial `os:` tag semantics: every host against every tag shape.

use crate::task::{HostOs, TaskOsRequirement, validate_os_tags};

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
