use std::collections::{HashMap, HashSet, VecDeque};

use super::*;

struct FixtureHost {
    release: String,
    probes: VecDeque<BwrapProbeOutcome>,
    files: HashMap<String, Vec<u8>>,
    profiles: Option<String>,
    commands: HashSet<String>,
    calls: Vec<String>,
    deny_authority: bool,
    authorized: bool,
    fail_command: Option<String>,
}

impl FixtureHost {
    fn new(id: &str, version: &str, details: &[&str]) -> Self {
        Self {
            release: format!("ID={id}\nVERSION_ID=\"{version}\"\n"),
            probes: details
                .iter()
                .map(|detail| BwrapProbeOutcome {
                    available: *detail == "ready",
                    trusted_path: "/usr/bin/bwrap".to_string(),
                    detail: (*detail).to_string(),
                })
                .collect(),
            files: HashMap::new(),
            profiles: Some(String::new()),
            commands: [
                "/usr/bin/apt-get",
                "/usr/bin/dnf",
                "/usr/bin/dnf5",
                "/usr/bin/pacman",
                "/usr/bin/install",
                "/usr/sbin/apparmor_parser",
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
            calls: Vec::new(),
            deny_authority: false,
            authorized: false,
            fail_command: None,
        }
    }
}

impl Host for FixtureHost {
    fn os_release(&self) -> Result<String, OrbitError> {
        Ok(self.release.clone())
    }

    fn probe(&mut self) -> BwrapProbeOutcome {
        self.probes
            .pop_front()
            .expect("fixture supplied every probe outcome")
    }

    fn root_file(&self, path: &str) -> Result<Option<Vec<u8>>, OrbitError> {
        Ok(self.files.get(path).cloned())
    }

    fn loaded_profiles(&self) -> Result<Option<String>, OrbitError> {
        Ok(self.profiles.clone())
    }

    fn has_command(&self, path: &str) -> bool {
        self.commands.contains(path)
    }

    fn authorize(&mut self, non_interactive: bool) -> Result<(), OrbitError> {
        if self.authorized {
            return Ok(());
        }
        self.calls
            .push(format!("authorize noninteractive={non_interactive}"));
        if self.deny_authority {
            Err(OrbitError::Execution(
                "administrator authentication denied".to_string(),
            ))
        } else {
            self.authorized = true;
            Ok(())
        }
    }

    fn run_privileged(&mut self, path: &str, args: &[&str]) -> Result<(), OrbitError> {
        self.calls.push(format!("{path} {}", args.join(" ")));
        if self.fail_command.as_deref() == Some(path) {
            return Err(OrbitError::Execution(format!("{path} failed")));
        }
        if path == "/usr/bin/apt-get" && args.contains(&"apparmor-profiles") {
            self.files
                .insert(PROFILE_SOURCE.to_string(), b"packaged profile".to_vec());
        }
        if path == "/usr/bin/install" {
            self.files
                .insert(PROFILE_TARGET.to_string(), b"packaged profile".to_vec());
        }
        if path == "/usr/sbin/apparmor_parser" {
            self.profiles = Some("bwrap (enforce)\n".to_string());
        }
        Ok(())
    }
}

const MISSING: &str = "trusted Bubblewrap not available at /usr/bin/bwrap";
const UID_DENIAL: &str =
    "Bubblewrap capability probe failed: setting up uid map: Permission denied";
const NO_USERNS: &str =
    "Bubblewrap capability probe failed: No permissions to create new namespace";
const OLD_BWRAP: &str = "Bubblewrap does not support the required --bind-fd object-authority mount";

#[test]
fn ready_host_is_idempotent_even_when_distribution_is_unknown() {
    let mut host = FixtureHost::new("mystery", "1", &["ready"]);
    assert!(
        prepare_with(&mut host, true)
            .unwrap()
            .contains("no host changes")
    );
    assert!(host.calls.is_empty());
}

#[test]
fn ubuntu_missing_prerequisites_installs_only_packaged_profile_then_reprobes() {
    let mut host = FixtureHost::new("ubuntu", "24.04", &[MISSING, UID_DENIAL, "ready"]);
    let result = prepare_with(&mut host, true).unwrap();
    assert!(result.contains("ready"));
    assert_eq!(
        host.calls,
        [
            "authorize noninteractive=true",
            "/usr/bin/apt-get update",
            "/usr/bin/apt-get install --yes bubblewrap apparmor-profiles",
            "/usr/bin/install -m 0644 /usr/share/apparmor/extra-profiles/bwrap-userns-restrict /etc/apparmor.d/bwrap-userns-restrict",
            "/usr/sbin/apparmor_parser -r /etc/apparmor.d/bwrap-userns-restrict",
        ]
    );
}

#[test]
fn package_matrix_selects_native_manager_and_refuses_old_versions() {
    for (id, version, command) in [
        ("debian", "13", "/usr/bin/apt-get install --yes bubblewrap"),
        ("fedora", "43", "/usr/bin/dnf5 -y install bubblewrap"),
        ("rocky", "10.1", "/usr/bin/dnf5 -y install bubblewrap"),
        (
            "arch",
            "",
            "/usr/bin/pacman -S --needed --noconfirm bubblewrap",
        ),
    ] {
        let mut host = FixtureHost::new(id, version, &[MISSING, "ready"]);
        assert!(prepare_with(&mut host, true).unwrap().contains("ready"));
        assert!(
            host.calls.iter().any(|call| call == command),
            "{id}: {:?}",
            host.calls
        );
    }
    for (id, version) in [
        ("debian", "12"),
        ("rhel", "9.7"),
        ("ubuntu", "22.04"),
        ("unknown", "1"),
    ] {
        let mut host = FixtureHost::new(id, version, &[MISSING]);
        assert!(
            prepare_with(&mut host, true)
                .unwrap_err()
                .to_string()
                .contains("does not support")
        );
        assert!(host.calls.is_empty(), "{id} {version} changed the host");
    }
}

#[test]
fn unavailable_privilege_and_package_failure_stop_before_false_readiness() {
    let mut denied = FixtureHost::new("debian", "13", &[MISSING]);
    denied.deny_authority = true;
    assert!(prepare_with(&mut denied, true).is_err());
    assert_eq!(denied.calls, ["authorize noninteractive=true"]);

    let mut package_failure = FixtureHost::new("fedora", "44", &[MISSING]);
    package_failure.fail_command = Some("/usr/bin/dnf5".to_string());
    assert!(prepare_with(&mut package_failure, false).is_err());
    assert_eq!(
        package_failure.calls.last().unwrap(),
        "/usr/bin/dnf5 -y install bubblewrap"
    );
}

#[test]
fn kernel_denial_and_old_package_refuse_without_profile_workaround() {
    let mut nested = FixtureHost::new("ubuntu", "24.04", &[NO_USERNS]);
    assert!(
        prepare_with(&mut nested, true)
            .unwrap_err()
            .to_string()
            .contains("kernel or enclosing container")
    );
    assert!(nested.calls.is_empty());

    // A container that caps user namespaces fails `unshare` with ENOSPC; no
    // package or profile can lift that either.
    let mut capped = FixtureHost::new(
        "ubuntu",
        "24.04",
        &[
            "Bubblewrap capability probe failed: bwrap: Creating new namespace failed: \
           nesting depth or /proc/sys/user/max_*_namespaces exceeded (ENOSPC)",
        ],
    );
    assert!(
        prepare_with(&mut capped, true)
            .unwrap_err()
            .to_string()
            .contains("kernel or enclosing container")
    );
    assert!(capped.calls.is_empty());

    let mut other_denial = FixtureHost::new(
        "ubuntu",
        "24.04",
        &[
            "Bubblewrap capability probe failed: mount denied",
            "Bubblewrap capability probe failed: mount denied",
            "Bubblewrap capability probe failed: mount denied",
        ],
    );
    other_denial
        .files
        .insert(PROFILE_SOURCE.to_string(), b"packaged profile".to_vec());
    assert!(prepare_with(&mut other_denial, true).is_err());
    assert!(other_denial.calls.is_empty());

    let mut old = FixtureHost::new("ubuntu", "24.04", &[OLD_BWRAP, OLD_BWRAP, OLD_BWRAP]);
    assert!(
        prepare_with(&mut old, true)
            .unwrap_err()
            .to_string()
            .contains("not ready")
    );
    assert!(
        !old.calls
            .iter()
            .any(|call| call.contains("apparmor_parser"))
    );
}

#[test]
fn custom_profile_conflict_and_loaded_profile_are_preserved() {
    let mut conflict = FixtureHost::new("ubuntu", "24.04", &[UID_DENIAL, UID_DENIAL]);
    conflict
        .files
        .insert(PROFILE_SOURCE.to_string(), b"packaged profile".to_vec());
    conflict
        .files
        .insert(PROFILE_TARGET.to_string(), b"custom profile".to_vec());
    assert!(
        prepare_with(&mut conflict, true)
            .unwrap_err()
            .to_string()
            .contains("differs")
    );
    assert!(conflict.calls.is_empty());

    let mut loaded = FixtureHost::new("ubuntu", "24.04", &[UID_DENIAL, UID_DENIAL]);
    loaded
        .files
        .insert(PROFILE_SOURCE.to_string(), b"packaged profile".to_vec());
    loaded.profiles = Some("bwrap (enforce)\n".to_string());
    assert!(
        prepare_with(&mut loaded, true)
            .unwrap_err()
            .to_string()
            .contains("already loaded")
    );
    assert!(loaded.calls.is_empty());
}
