use super::*;
use crate::credential_paths::credential_read_denies;
use credentials::append_credential_masks;

struct HomeFixture {
    _temp: tempfile::TempDir,
    home: PathBuf,
}

/// A fake HOME holding `.ssh`, `.aws`, `.config/gh` directories and a cargo
/// `credentials.toml` file, but no `.cargo/credentials` and no macOS trees.
fn home_fixture() -> HomeFixture {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().canonicalize().expect("canonical root");
    let home = root.join("home");
    for dir in [".ssh", ".aws", ".config/gh", ".cargo"] {
        fs::create_dir_all(home.join(dir)).expect("create credential dir");
    }
    fs::write(home.join(".ssh/id_ed25519"), b"secret").expect("key");
    fs::write(home.join(".cargo/credentials.toml"), b"token").expect("token");
    HomeFixture { _temp: temp, home }
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
