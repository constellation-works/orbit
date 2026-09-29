use std::cell::Cell;

use super::*;
use crate::credential_paths::credential_read_denies;
use argv::compile_plan_with_credentials;
use credentials::append_credential_masks;

struct HomeFixture {
    _temp: tempfile::TempDir,
    home: PathBuf,
    workspace: PathBuf,
}

/// A fake HOME holding `.ssh`, `.aws`, `.config/gh` directories and a cargo
/// `credentials.toml` file, but no `.cargo/credentials` and no macOS trees.
fn home_fixture() -> HomeFixture {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().canonicalize().expect("canonical root");
    let home = root.join("home");
    let workspace = root.join("workspace");
    for dir in [".ssh", ".aws", ".config/gh", ".cargo"] {
        fs::create_dir_all(home.join(dir)).expect("create credential dir");
    }
    fs::create_dir_all(&workspace).expect("workspace");
    fs::write(home.join(".ssh/id_ed25519"), b"secret").expect("key");
    fs::write(home.join(".cargo/credentials.toml"), b"token").expect("token");
    HomeFixture {
        _temp: temp,
        home,
        workspace,
    }
}

/// The denies resolved from a fixture `home`, limited to entries under it.
/// The absolute system keychain trees exist on a macOS host running these
/// tests, so leaving them in would make the outcome depend on the host.
fn home_denies(home: &Path) -> Vec<CredentialReadDeny> {
    credential_read_denies(Some(home.as_os_str()), None)
        .into_iter()
        .filter(|deny| deny.path.starts_with(home))
        .collect()
}

fn denies(fixture: &HomeFixture) -> Vec<CredentialReadDeny> {
    home_denies(&fixture.home)
}

fn no_mounts() -> Result<Vec<MountEntry>, OrbitError> {
    Ok(Vec::new())
}

fn has_pair(args: &[String], option: &str, path: &Path) -> bool {
    args.windows(2)
        .any(|pair| pair[0] == option && pair[1] == path.display().to_string())
}

fn has_null_bind(args: &[String], path: &Path) -> bool {
    args.windows(3).any(|triple| {
        triple[0] == "--ro-bind"
            && triple[1] == "/dev/null"
            && triple[2] == path.display().to_string()
    })
}

#[test]
fn existing_credential_dirs_and_files_are_masked_and_missing_ones_skipped() {
    let fixture = home_fixture();
    let mut out = Vec::new();

    append_credential_masks(&mut out, &denies(&fixture), no_mounts).expect("masks");

    for dir in [".ssh", ".aws", ".config/gh"] {
        assert!(
            has_pair(&out, "--tmpfs", &fixture.home.join(dir)),
            "credential directory `{dir}` must be replaced by an empty tmpfs: {out:?}"
        );
    }
    assert!(
        has_null_bind(&out, &fixture.home.join(".cargo/credentials.toml")),
        "the cargo token file must be bound over with /dev/null: {out:?}"
    );
    let rendered = out.join(" ");
    assert!(
        !rendered.contains(".cargo/credentials ") && !rendered.ends_with(".cargo/credentials"),
        "an absent legacy token file must be skipped, not mounted: {out:?}"
    );
    assert!(
        !rendered.contains("Keychains") && !rendered.contains("Chrome"),
        "absent macOS trees must be skipped: {out:?}"
    );
    assert_eq!(
        out.iter().filter(|arg| arg.as_str() == "--tmpfs").count(),
        3,
        "exactly the three existing directories are masked: {out:?}"
    );
}

#[test]
fn a_symlinked_credential_dir_masks_its_real_location() {
    let fixture = home_fixture();
    let dotfiles = fixture.home.join("dotfiles-ssh");
    fs::create_dir_all(&dotfiles).expect("dotfiles");
    fs::remove_dir_all(fixture.home.join(".ssh")).expect("remove .ssh");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&dotfiles, fixture.home.join(".ssh")).expect("symlink");
    let mut out = Vec::new();

    append_credential_masks(&mut out, &denies(&fixture), no_mounts).expect("masks");

    #[cfg(unix)]
    assert!(
        has_pair(&out, "--tmpfs", &dotfiles),
        "the mount must land on the symlink target, where the bytes live: {out:?}"
    );
}

#[test]
fn nothing_existing_emits_no_mount_and_never_reads_the_mount_table() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("empty-home");
    let denies = home_denies(&home);
    let read = Cell::new(false);
    let mut out = vec!["--die-with-parent".to_string()];

    append_credential_masks(&mut out, &denies, || {
        read.set(true);
        Ok(Vec::new())
    })
    .expect("nothing to mask");

    assert_eq!(out, vec!["--die-with-parent".to_string()]);
    assert!(
        !read.get(),
        "no credential exists, so no alias check is needed"
    );
}

#[test]
fn a_second_path_to_a_credential_dir_refuses_the_plan() {
    let fixture = home_fixture();
    let ssh = fixture.home.join(".ssh");
    let mut out = vec![
        "--bind".to_string(),
        ssh.join("keys").display().to_string(),
        "/mnt/elsewhere".to_string(),
    ];

    let error = append_credential_masks(&mut out, &denies(&fixture), no_mounts)
        .expect_err("an alias bind would leave the mask incomplete");

    assert!(
        matches!(&error, OrbitError::PolicyDenied(message) if message.contains("/mnt/elsewhere")),
        "the refusal names the alias: {error}"
    );
}

#[test]
fn a_grant_inside_a_credential_dir_refuses_the_plan_instead_of_hiding_it() {
    let fixture = home_fixture();
    let granted = fixture.home.join(".config/gh/workdir");
    fs::create_dir_all(&granted).expect("granted dir");
    let mut out = vec![
        "--bind".to_string(),
        granted.display().to_string(),
        granted.display().to_string(),
    ];

    let error = append_credential_masks(&mut out, &denies(&fixture), no_mounts)
        .expect_err("masking would hide a path the plan grants");

    assert!(
        matches!(&error, OrbitError::PolicyDenied(message) if message.contains("workdir")),
        "the refusal names the granted path: {error}"
    );
}

/// The workspace stays a writable bind, and every credential mask comes after
/// every other mount so no earlier grant can expose one again.
#[test]
fn compiled_plan_masks_credentials_last_and_keeps_the_worktree_writable() {
    let fixture = home_fixture();
    let resolved = profile(vec![format!("{}/**", fixture.workspace.display())]);

    let plan = compile_plan_with_credentials(
        &resolved,
        "/bin/true",
        &[],
        Some(&fixture.workspace),
        true,
        None,
        &denies(&fixture),
        no_mounts,
    )
    .expect("plan");

    let args = &plan.args;
    let workspace = fixture.workspace.display().to_string();
    let workspace_bind = args
        .windows(3)
        .position(|triple| {
            triple[0] == "--bind" && triple[1] == workspace && triple[2] == workspace
        })
        .expect("the worktree is bound writable");
    let home_prefix = fixture.home.display().to_string();
    let first_mask = args
        .windows(2)
        .position(|pair| pair[0] == "--tmpfs" && pair[1].starts_with(&home_prefix))
        .expect("credential masks are present");
    let last_other_mount = args
        .windows(3)
        .rposition(|triple| {
            matches!(triple[0].as_str(), "--bind" | "--ro-bind")
                && !triple[1].starts_with(&home_prefix)
                && triple[1] != "/dev/null"
        })
        .expect("policy mounts");
    assert!(workspace_bind < first_mask);
    assert!(
        last_other_mount < first_mask,
        "credential masks must follow every policy and alias mount: {args:?}"
    );
    let chdir = args.iter().position(|arg| arg == "--chdir").expect("chdir");
    assert!(first_mask < chdir, "masks precede the exec: {args:?}");
    assert!(
        !args
            .windows(2)
            .any(|pair| pair[0] == "--tmpfs" && pair[1] == workspace),
        "the worktree itself is never masked: {args:?}"
    );
}

#[test]
fn credential_masks_are_the_only_difference_from_an_unmasked_plan() {
    let fixture = home_fixture();
    let resolved = profile(vec![format!("{}/**", fixture.workspace.display())]);
    let compile = |credentials: &[CredentialReadDeny]| {
        compile_plan_with_credentials(
            &resolved,
            "/bin/true",
            &[],
            Some(&fixture.workspace),
            true,
            None,
            credentials,
            no_mounts,
        )
        .expect("plan")
    };

    let plain = compile(&[]);
    let masked = compile(&denies(&fixture));

    let home_prefix = fixture.home.display().to_string();
    let mut stripped = Vec::new();
    let mut index = 0;
    while index < masked.args.len() {
        let arg = masked.args[index].as_str();
        let is_dir_mask = arg == "--tmpfs" && masked.args[index + 1].starts_with(&home_prefix);
        let is_file_mask = arg == "--ro-bind"
            && masked.args[index + 1] == "/dev/null"
            && masked.args[index + 2].starts_with(&home_prefix);
        if is_dir_mask {
            index += 2;
        } else if is_file_mask {
            index += 3;
        } else {
            stripped.push(masked.args[index].clone());
            index += 1;
        }
    }
    assert_eq!(
        stripped, plain.args,
        "removing the credential masks must leave the unmasked plan intact"
    );
}
