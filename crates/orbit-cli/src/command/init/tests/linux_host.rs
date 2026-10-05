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
        self.authorized = true;
        Ok(())
    }

    fn run_privileged(&mut self, path: &str, args: &[&str]) -> Result<(), OrbitError> {
        self.calls.push(format!("{path} {}", args.join(" ")));
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
