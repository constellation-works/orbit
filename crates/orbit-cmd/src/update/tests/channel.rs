use std::path::{Path, PathBuf};

use crate::update::channel::{InstallChannel, release_archive_name, release_target_triple};

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
            InstallChannel::Homebrew,
            "brew upgrade orbit",
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
