use std::path::{Path, PathBuf};

use orbit_common::OrbitError;

use crate::update::channel::{
    CANONICAL_HOMEBREW_FORMULA, HomebrewInventory, InstallChannel, LEGACY_HOMEBREW_FORMULA,
    SystemHomebrewInventory, homebrew_remediation, release_archive_name, release_target_triple,
};

fn detect(path: &str, managed: Option<&str>) -> InstallChannel {
    InstallChannel::detect(Path::new(path), managed.map(Path::new))
}

#[test]
fn the_managed_install_directory_is_the_only_updatable_channel() {
    let channel = detect("/home/dev/.orbit/bin/orbit", Some("/home/dev/.orbit/bin"));

    assert_eq!(
        channel,
        InstallChannel::Managed {
            install_dir: PathBuf::from("/home/dev/.orbit/bin")
        }
    );
    assert_eq!(channel.as_str(), "managed");
    assert!(
        channel
            .unsupported_reason(Path::new("/home/dev/.orbit/bin/orbit"), "0.19.0")
            .is_none()
    );
}

#[test]
fn package_manager_installs_are_classified_and_get_their_own_command() {
    let cases = [
        (
            "/usr/lib/node_modules/@orbit-tools/cli/binaries/orbit",
            InstallChannel::Npm,
            "npm install -g @orbit-tools/cli@0.19.0",
        ),
        (
            "/opt/homebrew/Cellar/orbit/0.18.0/bin/orbit",
            InstallChannel::Homebrew { remediation: None },
            "brew upgrade constellation-works/tap/orbit",
        ),
        (
            "/home/dev/.cargo/bin/orbit",
            InstallChannel::Cargo,
            "cargo install",
        ),
        (
            "/home/dev/src/orbit/target/debug/orbit",
            InstallChannel::LocalBuild,
            "make install",
        ),
        ("/opt/custom/orbit", InstallChannel::Unknown, "install.sh"),
    ];

    for (path, expected, remediation) in cases {
        let channel = detect(path, Some("/home/dev/.orbit/bin"));
        assert_eq!(channel, expected, "{path}");
        let error = channel
            .unsupported_reason(Path::new(path), "0.19.0")
            .unwrap_or_else(|| panic!("{path} must not be updatable in place"));
        let message = error.to_string();
        assert!(message.contains(remediation), "{path}: {message}");
        assert!(message.contains(path), "{path}: {message}");
    }
}

#[test]
fn an_explicit_managed_directory_outranks_the_shape_of_the_path() {
    // An operator who points ORBIT_INSTALL_DIR at a cargo bin directory has
    // said who owns it; the heuristic must not overrule that statement.
    assert_eq!(
        detect("/home/dev/.cargo/bin/orbit", Some("/home/dev/.cargo/bin")),
        InstallChannel::Managed {
            install_dir: PathBuf::from("/home/dev/.cargo/bin")
        }
    );
}

#[test]
fn a_cross_compiled_build_tree_is_still_a_local_build() {
    assert_eq!(
        detect(
            "/src/orbit/target/aarch64-unknown-linux-gnu/release/orbit",
            None
        ),
        InstallChannel::LocalBuild
    );
}

#[test]
fn homebrew_remediation_names_the_ordinary_qualified_upgrade_for_a_canonical_install() {
    let text = homebrew_remediation(Ok(vec![CANONICAL_HOMEBREW_FORMULA.to_string()]));

    assert!(
        text.contains(&format!(
            "brew update && brew upgrade {CANONICAL_HOMEBREW_FORMULA}"
        )),
        "{text}"
    );
    assert!(!text.contains("uninstall"), "{text}");
}

#[test]
fn homebrew_remediation_migrates_a_legacy_only_install_off_the_retired_tap() {
    let text = homebrew_remediation(Ok(vec![LEGACY_HOMEBREW_FORMULA.to_string()]));

    assert!(
        text.contains(&format!("brew uninstall {LEGACY_HOMEBREW_FORMULA}")),
        "{text}"
    );
    assert!(
        text.contains(&format!("brew install {CANONICAL_HOMEBREW_FORMULA}")),
        "{text}"
    );
}

#[test]
fn homebrew_remediation_migrates_rather_than_upgrades_when_both_taps_are_installed() {
    let text = homebrew_remediation(Ok(vec![
        LEGACY_HOMEBREW_FORMULA.to_string(),
        CANONICAL_HOMEBREW_FORMULA.to_string(),
    ]));

    // The conflict is that both are installed at once; removing the legacy
    // one is the fix, not reinstalling the canonical one that is already there.
    assert!(
        text.contains(&format!("brew uninstall {LEGACY_HOMEBREW_FORMULA}")),
        "{text}"
    );
}

#[test]
fn homebrew_remediation_falls_back_to_the_canonical_upgrade_when_the_formula_is_unlisted() {
    let text = homebrew_remediation(Ok(vec!["jq".to_string()]));

    assert!(
        text.contains(&format!(
            "brew update && brew upgrade {CANONICAL_HOMEBREW_FORMULA}"
        )),
        "{text}"
    );
    assert!(!text.contains("uninstall"), "{text}");
}

#[test]
fn homebrew_remediation_stays_actionable_and_honest_when_brew_itself_fails() {
    let text = homebrew_remediation(Err(OrbitError::Execution(
        "brew: command not found".to_string(),
    )));

    assert!(
        text.contains(&format!(
            "brew update && brew upgrade {CANONICAL_HOMEBREW_FORMULA}"
        )),
        "{text}"
    );
    assert!(text.contains("brew: command not found"), "{text}");
    assert!(!text.contains("uninstall"), "{text}");
}

/// A [`HomebrewInventory`] built from a fixed answer, so
/// `detect_with_homebrew_ownership` can be exercised without a real `brew`.
struct FakeInventory {
    names: Option<Vec<String>>,
    error: Option<String>,
}

impl HomebrewInventory for FakeInventory {
    fn installed_full_names(&self) -> Result<Vec<String>, OrbitError> {
        match &self.error {
            Some(message) => Err(OrbitError::Execution(message.clone())),
            None => Ok(self.names.clone().unwrap_or_default()),
        }
    }
}

#[test]
fn detect_with_homebrew_ownership_resolves_the_legacy_migration() {
    let inventory = FakeInventory {
        names: Some(vec![LEGACY_HOMEBREW_FORMULA.to_string()]),
        error: None,
    };

    let channel = InstallChannel::detect_with_homebrew_ownership(
        Path::new("/opt/homebrew/Cellar/orbit/0.18.0/bin/orbit"),
        Some(Path::new("/home/dev/.orbit/bin")),
        &inventory,
    );

    let InstallChannel::Homebrew { remediation } = channel else {
        panic!("expected a Homebrew channel");
    };
    assert!(
        remediation
            .as_deref()
            .is_some_and(|text| text.contains(&format!(
                "brew uninstall {LEGACY_HOMEBREW_FORMULA}"
            ))),
        "{remediation:?}"
    );
}

#[test]
fn detect_with_homebrew_ownership_never_probes_a_non_homebrew_channel() {
    struct PanicsIfCalled;
    impl HomebrewInventory for PanicsIfCalled {
        fn installed_full_names(&self) -> Result<Vec<String>, OrbitError> {
            panic!("must not probe brew for a non-Homebrew channel");
        }
    }

    let channel = InstallChannel::detect_with_homebrew_ownership(
        Path::new("/home/dev/.cargo/bin/orbit"),
        Some(Path::new("/home/dev/.orbit/bin")),
        &PanicsIfCalled,
    );

    assert_eq!(channel, InstallChannel::Cargo);
}

#[test]
fn system_inventory_parses_full_names_from_a_controlled_brew_fixture() {
    let fixture_dir = tempfile::tempdir().expect("fixture dir");
    let brew = fixture_dir.path().join("brew");
    std::fs::write(
        &brew,
        format!(
            "#!/bin/sh\necho '{LEGACY_HOMEBREW_FORMULA}'\necho 'jq'\necho '{CANONICAL_HOMEBREW_FORMULA}'\n"
        ),
    )
    .expect("write fake brew");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&brew, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }

    let inventory = SystemHomebrewInventory { command: brew };
    let names = inventory
        .installed_full_names()
        .expect("controlled brew fixture succeeds");

    assert_eq!(
        names,
        vec![
            LEGACY_HOMEBREW_FORMULA.to_string(),
            "jq".to_string(),
            CANONICAL_HOMEBREW_FORMULA.to_string(),
        ]
    );
}

#[test]
fn system_inventory_reports_a_failing_brew_without_claiming_success() {
    let fixture_dir = tempfile::tempdir().expect("fixture dir");
    let brew = fixture_dir.path().join("brew");
    std::fs::write(
        &brew,
        "#!/bin/sh\necho 'Error: Homebrew is broken' >&2\nexit 1\n",
    )
    .expect("write failing fake brew");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&brew, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }

    let inventory = SystemHomebrewInventory { command: brew };
    let error = inventory
        .installed_full_names()
        .expect_err("a failing brew must not report success");

    assert!(error.to_string().contains("Homebrew is broken"), "{error}");
}

#[test]
fn the_release_asset_name_matches_what_the_installer_downloads() {
    let target = release_target_triple().expect("this platform publishes releases");

    assert_eq!(
        release_archive_name(target),
        format!("orbit-{target}.tar.gz")
    );
    assert!(
        [
            "x86_64-unknown-linux-gnu",
            "aarch64-unknown-linux-gnu",
            "aarch64-apple-darwin",
            "x86_64-apple-darwin"
        ]
        .contains(&target),
        "{target}"
    );
}
