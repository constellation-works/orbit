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
    bundled_staged: bool,
    /// Install the bundled binary with bytes other than the staged ones.
    tamper_install: bool,
    digests: HashMap<String, String>,
}

const STAGED: &str = "/fixture/staged/bwrap";
const STAGED_SHA256: &str = "5ea1ed";
const PACKAGE_COMMANDS: [&str; 5] = [
    "/usr/bin/apt-get",
    "/usr/bin/dnf5",
    "/usr/bin/dnf",
    "/usr/bin/pacman",
    "/usr/bin/zypper",
];

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
                "/usr/bin/zypper",
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
            bundled_staged: false,
            tamper_install: false,
            digests: HashMap::new(),
        }
    }

    fn without_package_managers(&mut self) {
        self.commands
            .retain(|command| !PACKAGE_COMMANDS.contains(&command.as_str()));
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
        self.bundled_staged = true;
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
            "/usr/bin/apt-get install --yes bubblewrap",
            vec![
                "/usr/bin/apt-get update",
                "/usr/bin/apt-get install --yes bubblewrap",
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
    for (id, version) in [
        ("ubuntu", "24.04"),
        ("ubuntu", "25.10"),
        ("debian", "12"),
        ("unknown", "1"),
    ] {
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
                host.calls.is_empty() && !host.bundled_staged,
                "{id} {version}: namespace denial and ready hosts must not attempt a package, \
                 profile, or bundled Bubblewrap remedy"
            );
        }
    }
}

const MISSING: &str = "trusted Bubblewrap not available at /usr/bin/bwrap";
const UID_MAP: &str = "bwrap: setting up uid map: Permission denied";
const NAMESPACE_DENIED: &str = "bwrap: Creating new namespace failed: Operation not permitted";

// Safety invariant: root must never probe as root or change packages without
// identifying the intended unprivileged user. Pure inputs avoid process-wide
// environment mutation or requiring this suite to run as root.
#[test]
fn root_preparation_requires_a_valid_unprivileged_sudo_identity() {
    for (uid, gid) in [
        (None, None),
        (None, Some("1000")),
        (Some("0"), Some("1000")),
        (Some("invalid"), Some("1000")),
        (Some("1000"), None),
        (Some("1000"), Some("invalid")),
    ] {
        assert!(
            intended_probe_user(0, uid, gid).is_err(),
            "{uid:?}, {gid:?}"
        );
    }
    assert_eq!(
        intended_probe_user(0, Some("1000"), Some("1001")).unwrap(),
        Some((1000, 1001))
    );
    assert_eq!(intended_probe_user(1000, None, None).unwrap(), None);
}

// Fault injection at the Host boundary: neither a derivative nor a different
// probe signature may cause Orbit to write or load Ubuntu's AppArmor profile.
#[test]
fn apparmor_remedy_requires_ubuntu_and_the_exact_uid_map_denial() {
    for (id, version, id_like, detail, remedy) in [
        ("ubuntu", "22.04", "debian", UID_MAP, true),
        ("ubuntu", "24.04", "debian", UID_MAP, true),
        ("ubuntu", "25.10", "debian", UID_MAP, true),
        ("linuxmint", "22", "ubuntu debian", UID_MAP, false),
        ("pop", "24.04", "ubuntu", UID_MAP, false),
        ("debian", "12", "", UID_MAP, false),
        ("unknown", "1", "", UID_MAP, false),
        (
            "ubuntu",
            "25.10",
            "debian",
            "bwrap: setting up uid map: Operation not permitted",
            false,
        ),
        ("ubuntu", "25.10", "debian", NAMESPACE_DENIED, false),
    ] {
        let final_detail = if remedy { "ready" } else { detail };
        let mut host = FixtureHost::new(id, version, &[detail, detail, final_detail]);
        host.release.push_str(&format!("ID_LIKE='{id_like}'\n"));
        host.files
            .insert(PROFILE_SOURCE.to_string(), b"packaged profile".to_vec());
        let result = prepare_with(&mut host, true);
        assert_eq!(result.is_ok(), remedy, "{id} {version}: {result:?}");
        if remedy {
            assert_eq!(host.files.get(PROFILE_TARGET).unwrap(), b"packaged profile");
            assert!(profile_is_loaded(host.profiles.as_deref().unwrap()));
            assert_eq!(host.calls.len(), 3, "{:?}", host.calls);
        } else {
            assert!(host.calls.is_empty(), "{id} {version}: {:?}", host.calls);
            assert!(!host.files.contains_key(PROFILE_TARGET));
            assert!(!host.bundled_staged);
            assert!(result.unwrap_err().to_string().contains(detail));
        }
    }
}

#[test]
fn existing_custom_or_loaded_apparmor_profiles_are_preserved() {
    for (target, profiles) in [
        (Some(b"custom profile".to_vec()), ""),
        (None, "bwrap (enforce)\n"),
        (Some(b"packaged profile".to_vec()), "bwrap (complain)\n"),
    ] {
        let mut host = FixtureHost::new("ubuntu", "25.10", &[UID_MAP, UID_MAP]);
        host.files
            .insert(PROFILE_SOURCE.to_string(), b"packaged profile".to_vec());
        if let Some(bytes) = &target {
            host.files.insert(PROFILE_TARGET.to_string(), bytes.clone());
        }
        host.profiles = Some(profiles.to_string());
        assert!(prepare_with(&mut host, true).is_err());
        assert_eq!(host.files.get(PROFILE_TARGET), target.as_ref());
        assert_eq!(host.profiles.as_deref(), Some(profiles));
        assert!(
            host.calls.is_empty(),
            "existing policy must not be overwritten or reloaded"
        );
    }
}

// Fault injection: package installation can change the probe failure; loading
// a security profile still requires the fresh Ubuntu UID-map signature.
#[test]
fn apparmor_package_install_rechecks_the_signature_before_loading_policy() {
    for initial in [MISSING, UID_MAP] {
        for after_package in [UID_MAP, "ready", NAMESPACE_DENIED, "bwrap: mount failed"] {
            let mut host = FixtureHost::new(
                "ubuntu",
                "25.10",
                &[initial, UID_MAP, after_package, "ready"],
            );
            let result = prepare_with(&mut host, true);
            let loads_profile = after_package == UID_MAP;
            assert_eq!(
                result.is_ok(),
                loads_profile || after_package == "ready",
                "{result:?}"
            );
            assert_eq!(host.files.contains_key(PROFILE_TARGET), loads_profile);
            assert_eq!(
                profile_is_loaded(host.profiles.as_deref().unwrap()),
                loads_profile
            );
            assert_eq!(
                host.calls
                    .iter()
                    .filter(|call| call.contains("apparmor_parser"))
                    .count(),
                usize::from(loads_profile)
            );
            assert!(
                host.calls
                    .iter()
                    .any(|call| call == "/usr/bin/apt-get install --yes apparmor-profiles")
            );
            assert_eq!(
                host.calls
                    .iter()
                    .any(|call| call == "/usr/bin/apt-get install --yes bubblewrap"),
                initial == MISSING
            );
            assert!(!host.bundled_staged);
            if let Err(error) = result {
                assert!(error.to_string().contains(after_package), "{error}");
            }
        }
    }
}

#[test]
fn package_selection_uses_available_commands_and_family_without_version_gates() {
    // Combinatorial decision logic at the Host boundary: all managers are
    // present so the selected command proves ID/ID_LIKE preference as well.
    for (id, version, id_like, command) in [
        (
            "debian",
            "12",
            "",
            "/usr/bin/apt-get install --yes bubblewrap",
        ),
        (
            "ubuntu",
            "25.10",
            "debian",
            "/usr/bin/apt-get install --yes bubblewrap",
        ),
        (
            "linuxmint",
            "22",
            "ubuntu debian",
            "/usr/bin/apt-get install --yes bubblewrap",
        ),
        (
            "pop",
            "24.04",
            "ubuntu",
            "/usr/bin/apt-get install --yes bubblewrap",
        ),
        ("fedora", "42", "", "/usr/bin/dnf5 -y install bubblewrap"),
        (
            "rhel",
            "9.7",
            "fedora",
            "/usr/bin/dnf5 -y install bubblewrap",
        ),
        (
            "derivative",
            "1",
            "unknown rhel fedora",
            "/usr/bin/dnf5 -y install bubblewrap",
        ),
        (
            "arch",
            "",
            "",
            "/usr/bin/pacman -S --needed --noconfirm bubblewrap",
        ),
        (
            "derivative",
            "1",
            "arch",
            "/usr/bin/pacman -S --needed --noconfirm bubblewrap",
        ),
        (
            "opensuse-tumbleweed",
            "20261001",
            "arch",
            "/usr/bin/zypper --non-interactive install bubblewrap",
        ),
        (
            "opensuse-leap",
            "16.0",
            "suse",
            "/usr/bin/zypper --non-interactive install bubblewrap",
        ),
        (
            "derivative",
            "1",
            "suse",
            "/usr/bin/zypper --non-interactive install bubblewrap",
        ),
    ] {
        let mut host = FixtureHost::new(id, version, &[MISSING, "ready"]);
        host.release.push_str(&format!("ID_LIKE=\"{id_like}\"\n"));
        assert!(prepare_with(&mut host, true).unwrap().contains("ready"));
        assert!(
            host.calls.iter().any(|call| call == command),
            "{id}: {:?}",
            host.calls
        );
        assert!(
            !host.bundled_staged,
            "a package that passes the probe must not be joined by the bundled binary"
        );
        assert!(
            !host.calls.iter().any(|call| call.contains("apparmor")),
            "a passing package must not trigger Ubuntu's profile remedy"
        );
    }
    // Availability wins even when os-release is unknown or the family
    // manager is absent. Exercise dnf without dnf5 and each fallback path.
    for (id, manager, command) in [
        (
            "unknown",
            "/usr/bin/apt-get",
            "/usr/bin/apt-get install --yes bubblewrap",
        ),
        (
            "fedora",
            "/usr/bin/dnf",
            "/usr/bin/dnf -y install bubblewrap",
        ),
        (
            "debian",
            "/usr/bin/dnf5",
            "/usr/bin/dnf5 -y install bubblewrap",
        ),
        (
            "unknown",
            "/usr/bin/pacman",
            "/usr/bin/pacman -S --needed --noconfirm bubblewrap",
        ),
        (
            "unknown",
            "/usr/bin/zypper",
            "/usr/bin/zypper --non-interactive install bubblewrap",
        ),
    ] {
        let mut host = FixtureHost::new(id, "1", &[MISSING, "ready"]);
        host.without_package_managers();
        host.commands.insert(manager.to_string());
        assert!(prepare_with(&mut host, true).is_ok());
        assert!(
            host.calls.iter().any(|call| call == command),
            "{:?}",
            host.calls
        );
        assert!(!host.bundled_staged);
    }
}

const NO_BIND_FD: &str = "Bubblewrap at /usr/bin/bwrap does not support the required --bind-fd \
                          and --ro-bind-fd object-authority mounts, and no bundled Bubblewrap is installed";
const BUNDLED_READY: &str = "ready-bundled:0.12.0";
const BUNDLED_INSTALL: [&str; 2] = [
    "/usr/bin/install -d -o root -g root -m 0755 /usr/local/libexec/orbit",
    "/usr/bin/install -o root -g root -m 0755 /fixture/staged/bwrap /usr/local/libexec/orbit/bwrap",
];

#[test]
fn an_old_host_bwrap_gets_the_bundled_one_and_its_failures_stop_before_sudo() {
    for detail in [MISSING, NO_BIND_FD] {
        let mut host = FixtureHost::new("ubuntu", "22.04", &[detail, BUNDLED_READY]);
        host.without_package_managers();
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
    host.without_package_managers();
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
    host.without_package_managers();
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
fn an_old_package_falls_through_to_the_bundled_binary_after_install_then_probe() {
    let mut host = FixtureHost::new("debian", "12", &[NO_BIND_FD, NO_BIND_FD, BUNDLED_READY]);
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
fn missing_bundled_release_keeps_the_capability_and_manager_gap_in_the_error() {
    for has_manager in [false, true] {
        for gap in [MISSING, NO_BIND_FD] {
            let probes = if has_manager {
                vec![gap, gap]
            } else {
                vec![gap]
            };
            let mut host = FixtureHost::new("debian", "12", &probes);
            if !has_manager {
                host.without_package_managers();
            }
            host.bundled = Err("release binary unavailable");
            let error = prepare_with(&mut host, true).unwrap_err().to_string();
            assert!(error.contains(gap), "{error}");
            assert!(error.contains("release binary unavailable"), "{error}");
            assert_eq!(
                error.contains("no supported package manager"),
                !has_manager,
                "{error}"
            );
            if has_manager {
                assert_eq!(host.calls.len(), 3, "{:?}", host.calls);
            } else {
                assert!(
                    host.calls.is_empty(),
                    "a missing release must stop before elevation"
                );
            }
        }
    }
}

#[test]
fn namespace_denial_after_install_never_tries_a_bundle_or_profile() {
    let mut host = FixtureHost::new("ubuntu", "25.10", &[MISSING, NAMESPACE_DENIED]);
    let error = prepare_with(&mut host, true).unwrap_err().to_string();
    assert!(error.contains("namespace creation is denied"), "{error}");
    assert!(error.contains(NAMESPACE_DENIED), "{error}");
    assert_eq!(
        host.calls.len(),
        3,
        "only Bubblewrap package installation may run"
    );
    assert!(!host.bundled_staged);
    assert!(!host.files.contains_key(PROFILE_TARGET));
}

#[test]
fn a_stale_bundled_binary_is_refreshed_to_this_release() {
    let mut host = FixtureHost::new("ubuntu", "22.04", &["ready-bundled:0.11.0", BUNDLED_READY]);
    let reason = prepare_with(&mut host, true).unwrap();
    assert!(reason.contains("refreshed from 0.11.0"), "{reason}");
    assert_eq!(host.calls[1..], BUNDLED_INSTALL);
}
