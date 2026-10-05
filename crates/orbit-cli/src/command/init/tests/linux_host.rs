use std::collections::{HashMap, HashSet, VecDeque};

use super::*;

struct FixtureHost {
    release: String,
    probes: VecDeque<BwrapProbeOutcome>,
    files: HashMap<String, Vec<u8>>,
    profiles: Option<String>,
    commands: HashSet<String>,
    calls: Vec<String>,
    authorized: bool,
    authorization_error: Option<&'static str>,
    fail_command: Option<&'static str>,
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
            authorized: false,
            authorization_error: None,
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
        if let Some(reason) = self.authorization_error {
            return Err(OrbitError::Execution(reason.to_string()));
        }
        self.authorized = true;
        Ok(())
    }

    fn run_privileged(&mut self, path: &str, args: &[&str]) -> Result<(), OrbitError> {
        let command = format!("{path} {}", args.join(" "));
        self.calls.push(command.clone());
        if self.fail_command == Some(command.as_str()) {
            return Err(OrbitError::Execution(
                "injected command failure".to_string(),
            ));
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

// Fault injection: prove preparation stops at declined/denied authorization,
// including the second authorization site used for Ubuntu's profile remedy.
#[test]
fn authorization_refusal_never_runs_a_privileged_command() {
    const UID_MAP: &str = "setting up uid map: Permission denied";
    for (initial, reason) in [
        (MISSING, "noninteractive sudo authentication was denied"),
        (UID_MAP, "sudo authentication was declined"),
    ] {
        for non_interactive in [false, true] {
            let probes = if initial == MISSING {
                vec![initial]
            } else {
                vec![initial, initial]
            };
            let mut host = FixtureHost::new("ubuntu", "24.04", &probes);
            host.files
                .insert(PROFILE_SOURCE.to_string(), b"packaged profile".to_vec());
            host.authorization_error = Some(reason);
            let error = prepare_with(&mut host, non_interactive)
                .unwrap_err()
                .to_string();
            assert!(error.contains(reason), "{error}");
            assert_eq!(
                host.calls,
                [format!("authorize noninteractive={non_interactive}")],
                "a refused sudo prompt must stop all privileged preparation"
            );
        }
    }
}

// Fault injection: a failed package/profile command leaves prior commands in
// the diagnostic and prevents all subsequent preparation commands.
#[test]
fn partial_host_changes_are_reported_and_preparation_stops_on_command_failure() {
    const UID_MAP: &str = "setting up uid map: Permission denied";
    for (probes, failed, attempted) in [
        (
            vec![MISSING],
            "/usr/bin/apt-get install --yes bubblewrap apparmor-profiles",
            vec![
                "/usr/bin/apt-get update",
                "/usr/bin/apt-get install --yes bubblewrap apparmor-profiles",
            ],
        ),
        (
            vec![UID_MAP, UID_MAP],
            "/usr/sbin/apparmor_parser -r /etc/apparmor.d/bwrap-userns-restrict",
            vec![
                "/usr/bin/install -m 0644 /usr/share/apparmor/extra-profiles/bwrap-userns-restrict /etc/apparmor.d/bwrap-userns-restrict",
                "/usr/sbin/apparmor_parser -r /etc/apparmor.d/bwrap-userns-restrict",
            ],
        ),
    ] {
        let mut host = FixtureHost::new("ubuntu", "24.04", &probes);
        host.files
            .insert(PROFILE_SOURCE.to_string(), b"packaged profile".to_vec());
        host.fail_command = Some(failed);
        let error = prepare_with(&mut host, true).unwrap_err().to_string();
        for command in &attempted {
            assert!(
                error.contains(command),
                "partial host changes must identify {command}: {error}"
            );
        }
        assert_eq!(
            &host.calls[1..],
            attempted,
            "failed preparation must not run further privileged commands"
        );
    }
}

#[test]
fn namespace_denial_and_ready_host_never_request_host_changes() {
    for detail in [
        "ready",
        "bwrap: No permissions to create new namespace",
        "bwrap: Creating new namespace failed: Operation not permitted",
    ] {
        let mut host = FixtureHost::new("ubuntu", "24.04", &[detail]);
        let result = prepare_with(&mut host, true);
        assert_eq!(result.is_ok(), detail == "ready");
        if let Err(error) = result {
            assert!(error.to_string().contains(detail), "{error}");
        }
        assert!(
            host.calls.is_empty(),
            "namespace denial/ready hosts must not attempt a package or profile remedy"
        );
    }
}

const MISSING: &str = "trusted Bubblewrap not available at /usr/bin/bwrap";

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
