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
    /// What staging the release's bundled Bubblewrap yields.
    bundled: Result<(String, String), &'static str>,
    /// Install the bundled binary with bytes other than the staged ones.
    tamper_install: bool,
    digests: HashMap<String, String>,
}

const STAGED: &str = "/tmp/orbit-bwrap-staged/bwrap";
const STAGED_SHA256: &str = "5ea1ed";

/// `ready` passes on the host binary; `ready-bundled:<version>` passes on the
/// bundled one; anything else is a failed probe with that detail.
fn probe_outcome(detail: &str) -> BwrapProbeOutcome {
    if let Some(version) = detail.strip_prefix("ready-bundled:") {
        return BwrapProbeOutcome {
            available: true,
            trusted_path: BUNDLED_BWRAP_PATH.to_string(),
            detail: "capability probe succeeded".to_string(),
            source: Some(BwrapSource::Bundled),
            version: Some(version.to_string()),
        };
    }
    BwrapProbeOutcome {
        available: detail == "ready",
        trusted_path: "/usr/bin/bwrap".to_string(),
        detail: detail.to_string(),
        source: (detail == "ready").then_some(BwrapSource::Host),
        version: (detail == "ready").then(|| "0.11.1".to_string()),
    }
}

impl FixtureHost {
    fn new(id: &str, version: &str, details: &[&str]) -> Self {
        Self {
            release: format!("ID={id}\nVERSION_ID=\"{version}\"\n"),
            probes: details.iter().map(|detail| probe_outcome(detail)).collect(),
            files: HashMap::new(),
            profiles: Some(String::new()),
            commands: [
                "/usr/bin/apt-get",
                "/usr/bin/dnf",
                "/usr/bin/dnf5",
                "/usr/bin/pacman",
                "/usr/bin/install",
                "/usr/bin/rm",
                "/usr/sbin/apparmor_parser",
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
            calls: Vec::new(),
            authorized: false,
            authorization_error: None,
            fail_command: None,
            bundled: Ok((STAGED.to_string(), STAGED_SHA256.to_string())),
            tamper_install: false,
            digests: HashMap::new(),
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
        if path == "/usr/bin/install" && args.contains(&PROFILE_TARGET) {
            self.files
                .insert(PROFILE_TARGET.to_string(), b"packaged profile".to_vec());
        }
        if path == "/usr/bin/install" && args.contains(&BUNDLED_BWRAP_PATH) {
            let digest = if self.tamper_install {
                "something-else"
            } else {
                STAGED_SHA256
            };
            self.digests
                .insert(BUNDLED_BWRAP_PATH.to_string(), digest.to_string());
        }
        if path == "/usr/sbin/apparmor_parser" {
            self.profiles = Some("bwrap (enforce)\n".to_string());
        }
        Ok(())
    }

    fn stage_bundled(&mut self) -> Result<(String, String), OrbitError> {
        self.bundled
            .clone()
            .map_err(|reason| OrbitError::Execution(reason.to_string()))
    }

    fn file_sha256(&self, path: &str) -> Result<Option<String>, OrbitError> {
        Ok(self.digests.get(path).cloned())
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
    let current_bundled = format!("ready-bundled:{BUNDLED_BWRAP_VERSION}");
    for (id, version) in [("ubuntu", "24.04"), ("ubuntu", "22.04")] {
        for detail in [
            "ready",
            current_bundled.as_str(),
            "bwrap: No permissions to create new namespace",
            "bwrap: Creating new namespace failed: Operation not permitted",
        ] {
            let mut host = FixtureHost::new(id, version, &[detail]);
            let result = prepare_with(&mut host, true);
            assert_eq!(result.is_ok(), detail.starts_with("ready"));
            match result {
                Ok(reason) => assert!(reason.contains("no host changes needed"), "{reason}"),
                Err(error) => assert!(error.to_string().contains(detail), "{error}"),
            }
            assert!(
                host.calls.is_empty(),
                "{id} {version}: namespace denial and ready hosts must not attempt a package, \
                 profile, or bundled Bubblewrap remedy"
            );
        }
    }
}

const MISSING: &str = "trusted Bubblewrap not available at /usr/bin/bwrap";

#[test]
fn package_matrix_selects_native_manager_and_bundles_for_old_versions() {
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
        assert!(
            !host
                .calls
                .iter()
                .any(|call| call.contains(BUNDLED_BWRAP_PATH)),
            "a package that passes the probe must not be joined by the bundled binary"
        );
    }
    // Without a package path, the release's bundled Bubblewrap is the only
    // remedy for a missing or incapable host binary.
    for (id, version) in [
        ("debian", "12"),
        ("rhel", "9.7"),
        ("ubuntu", "22.04"),
        ("unknown", "1"),
    ] {
        let mut host = FixtureHost::new(id, version, &[MISSING, BUNDLED_READY]);
        assert!(prepare_with(&mut host, true).unwrap().contains("bundled"));
        assert_eq!(host.calls[1..], BUNDLED_INSTALL, "{id} {version}");
    }
}

const NO_BIND_FD: &str = "Bubblewrap at /usr/bin/bwrap does not support the required --bind-fd \
                          object-authority mount, and no bundled Bubblewrap is installed";
const BUNDLED_READY: &str = "ready-bundled:0.12.0";
const BUNDLED_INSTALL: [&str; 2] = [
    "/usr/bin/install -d -o root -g root -m 0755 /usr/local/libexec/orbit",
    "/usr/bin/install -o root -g root -m 0755 /tmp/orbit-bwrap-staged/bwrap /usr/local/libexec/orbit/bwrap",
];

#[test]
fn an_old_host_bwrap_gets_the_bundled_one_and_its_failures_stop_before_sudo() {
    for detail in [MISSING, NO_BIND_FD] {
        let mut host = FixtureHost::new("ubuntu", "22.04", &[detail, BUNDLED_READY]);
        let reason = prepare_with(&mut host, false).unwrap();
        assert!(
            reason.contains("bundled Bubblewrap 0.12.0 at /usr/local/libexec/orbit/bwrap"),
            "{reason}"
        );
        assert_eq!(host.calls[0], "authorize noninteractive=false");
        assert_eq!(host.calls[1..], BUNDLED_INSTALL);
    }

    // An unsigned or mismatched release binary is refused while staging, so
    // no administrator prompt or privileged command follows.
    let mut host = FixtureHost::new("rhel", "9.4", &[NO_BIND_FD]);
    host.bundled = Err("release checksum signature verification failed");
    let error = prepare_with(&mut host, true).unwrap_err().to_string();
    assert!(error.contains("signature verification failed"), "{error}");
    assert!(host.calls.is_empty(), "{:?}", host.calls);
}

// Fault injection: bytes that differ from the verified staging file once
// installed as root are removed rather than left at the trusted path.
#[test]
fn an_installed_binary_that_differs_from_the_signed_digest_is_removed() {
    let mut host = FixtureHost::new("ubuntu", "22.04", &[MISSING]);
    host.tamper_install = true;
    let error = prepare_with(&mut host, true).unwrap_err().to_string();
    assert!(
        error.contains("does not match the signed release digest"),
        "{error}"
    );
    assert_eq!(
        host.calls.last().map(String::as_str),
        Some("/usr/bin/rm -f /usr/local/libexec/orbit/bwrap")
    );
}

#[test]
fn a_supported_distribution_tries_its_package_before_the_bundled_binary() {
    let mut host = FixtureHost::new("debian", "13", &[NO_BIND_FD, NO_BIND_FD, BUNDLED_READY]);
    assert!(prepare_with(&mut host, true).unwrap().contains("bundled"));
    assert_eq!(
        host.calls[1..],
        [
            "/usr/bin/apt-get update",
            "/usr/bin/apt-get install --yes bubblewrap",
            BUNDLED_INSTALL[0],
            BUNDLED_INSTALL[1],
        ]
    );
}

#[test]
fn a_stale_bundled_binary_is_refreshed_to_this_release() {
    let mut host = FixtureHost::new("ubuntu", "22.04", &["ready-bundled:0.11.0", BUNDLED_READY]);
    let reason = prepare_with(&mut host, true).unwrap();
    assert!(reason.contains("refreshed from 0.11.0"), "{reason}");
    assert_eq!(host.calls[1..], BUNDLED_INSTALL);
}
