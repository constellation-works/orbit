//! Root resolution for registered tools in `orbit-runtime`'s `tool_exec.rs`,
//! which needs a runtime composed by this crate.

use std::path::{Path, PathBuf};

use crate::OrbitRuntime;
use crate::runtime::tool_exec::populate_filesystem_policy_context;
use orbit_tools::ToolContext;
use tempfile::TempDir;

/// A source-inspection slot is a standalone repository, so it never shares the
/// runtime's common Git directory. [ORB-13800] It is still this repository's
/// pinned checkout, so registered tools run there; a repository merely planted
/// in or beside the pool still falls back to the registered root.
#[test]
fn root_resolution_uses_owned_inspection_slot_and_rejects_planted_checkouts() {
    let fixture = GitRuntimeFixture::new();
    let repo_root = canonical(&fixture.repo_root);
    let revision = git_stdout(&repo_root, &["rev-parse", "HEAD"]);
    let pool = repo_root.join(".orbit/state/source-inspections-v1");

    let slot = inspection_slot(&pool.join("0"), &repo_root.join(".git"), &revision);
    let subdirectory = slot.join("subdirectory");
    std::fs::create_dir(&subdirectory).expect("create slot subdirectory");
    for cwd in [&slot, &subdirectory] {
        assert_eq!(
            resolved_root(&fixture.runtime, cwd),
            Some(slot.clone()),
            "an owned inspection slot must be the process root"
        );
    }

    let foreign = inspection_slot(
        &pool.join("1"),
        &canonical(&fixture.unrelated).join(".git"),
        &git_stdout(&fixture.unrelated, &["rev-parse", "HEAD"]),
    );
    let unowned = inspection_slot(&pool.join("2"), &repo_root.join(".git"), &revision);
    std::fs::remove_file(pool.join("2/owner")).expect("drop owner marker");
    let outside_pool = inspection_slot(
        &repo_root.join(".orbit/state/elsewhere/0"),
        &repo_root.join(".git"),
        &revision,
    );
    for (cwd, case) in [
        (&foreign, "a slot holding another repository's history"),
        (&unowned, "a slot without its owner marker"),
        (&outside_pool, "a checkout outside the inspection pool"),
    ] {
        assert_eq!(
            resolved_root(&fixture.runtime, cwd),
            Some(repo_root.clone()),
            "{case} must not become the process root"
        );
    }
}

/// A failed first git probe must not poison the checkout for the process:
/// after git recovers, the same process resolves the owned inspection slot,
/// and the verified root is then memoised with no further git spawns.
#[cfg(unix)]
#[test]
fn failed_first_checkout_probe_recovers_to_inspection_slot_in_same_process() {
    if crate::application::run_isolated_test(std::any::type_name_of_val(
        &failed_first_checkout_probe_recovers_to_inspection_slot_in_same_process,
    )) {
        return;
    }
    let fixture = GitRuntimeFixture::new();
    let repo_root = canonical(&fixture.repo_root);
    let revision = git_stdout(&repo_root, &["rev-parse", "HEAD"]);
    let pool = repo_root.join(".orbit/state/source-inspections-v1");
    let slot = inspection_slot(&pool.join("0"), &repo_root.join(".git"), &revision);
    let count = install_fail_first_git(fixture._root.path());

    assert_eq!(
        resolved_root(&fixture.runtime, &slot),
        Some(repo_root.clone()),
        "a failed first probe falls back to the registered root"
    );
    assert_eq!(
        resolved_root(&fixture.runtime, &slot),
        Some(slot.clone()),
        "recovery in the same process must resolve the inspection checkout"
    );
    std::fs::write(&count, "").expect("reset git spawn count");
    assert_eq!(resolved_root(&fixture.runtime, &slot), Some(slot));
    assert_eq!(
        std::fs::read_to_string(&count)
            .map(|text| text.lines().count())
            .unwrap_or(0),
        0,
        "a memoised successful resolution must not spawn git"
    );
}

/// Install a `git` shim first on PATH that fails its first spawn, counts
/// every spawn, and delegates later spawns to the real git.
#[cfg(unix)]
fn install_fail_first_git(root: &Path) -> PathBuf {
    let real_git = std::env::var_os("PATH")
        .and_then(|paths| {
            std::env::split_paths(&paths).find_map(|dir| {
                let candidate = dir.join("git");
                candidate.is_file().then_some(candidate)
            })
        })
        .expect("real git on PATH");
    let shim_dir = root.join("fail-first-git-shim");
    std::fs::create_dir_all(&shim_dir).expect("shim dir");
    let count = root.join("fail-first-git-spawns");
    std::fs::write(&count, "").expect("count file");
    let failed = root.join("fail-first-git-failed");
    let shim = format!(
        "#!/bin/sh\nprintf 'x\\n' >> '{}'\nif [ ! -e '{}' ]; then : > '{}'; echo injected-probe-failure >&2; exit 128; fi\nexec '{}' \"$@\"\n",
        count.display(),
        failed.display(),
        failed.display(),
        real_git.display()
    );
    let shim_path = shim_dir.join("git");
    std::fs::write(&shim_path, shim).expect("shim");
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&shim_path, std::fs::Permissions::from_mode(0o755))
            .expect("shim permissions");
    }
    let mut paths = vec![shim_dir];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    // SAFETY: this test runs in an isolated child process (see
    // `run_isolated_test`) executing exactly one test, so no other thread
    // observes the PATH change.
    unsafe {
        std::env::set_var("PATH", std::env::join_paths(paths).expect("join PATH"));
    }
    count
}

fn resolved_root(runtime: &OrbitRuntime, cwd: &Path) -> Option<PathBuf> {
    let mut context = cwd_context(cwd, None, None);
    populate_filesystem_policy_context(runtime, &mut context).expect("populate context");
    context.workspace_root
}

/// The slot layout the CLI runner materializes: a standalone repository
/// fetched at `revision` and detached, beside its owner marker.
fn inspection_slot(slot: &Path, source_git_dir: &Path, revision: &str) -> PathBuf {
    let checkout = slot.join("checkout");
    std::fs::create_dir_all(&checkout).expect("create slot checkout");
    std::fs::write(slot.join("owner"), "orbit-source-inspection-v1\n").expect("owner marker");
    git(&checkout, &["init", "--quiet", "--template="]);
    git(
        &checkout,
        &[
            "-c",
            "protocol.file.allow=always",
            "fetch",
            "--quiet",
            "--no-tags",
            source_git_dir.to_str().expect("utf8 git dir"),
            revision,
        ],
    );
    git(&checkout, &["checkout", "--quiet", "--detach", revision]);
    canonical(&checkout)
}

struct GitRuntimeFixture {
    _root: TempDir,
    runtime: OrbitRuntime,
    repo_root: PathBuf,
    unrelated: PathBuf,
}

impl GitRuntimeFixture {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        let global_root = root.path().join("global");
        let repo_root = root.path().join("repo");
        let unrelated = root.path().join("unrelated");
        std::fs::create_dir_all(&global_root).expect("global");
        std::fs::create_dir_all(repo_root.join(".orbit")).expect("workspace");
        std::fs::create_dir_all(&unrelated).expect("unrelated");

        git(&repo_root, &["init", "-b", "agent-main"]);
        git(&repo_root, &["config", "user.name", "Orbit Test"]);
        git(
            &repo_root,
            &["config", "user.email", "orbit-test@example.com"],
        );
        git(&repo_root, &["config", "commit.gpgsign", "false"]);
        std::fs::write(repo_root.join("README.md"), "init\n").expect("write");
        git(&repo_root, &["add", "README.md"]);
        git(&repo_root, &["commit", "-m", "init"]);

        git(&unrelated, &["init", "-b", "other"]);
        git(&unrelated, &["config", "user.name", "Orbit Test"]);
        git(
            &unrelated,
            &["config", "user.email", "orbit-test@example.com"],
        );
        git(&unrelated, &["config", "commit.gpgsign", "false"]);
        std::fs::write(unrelated.join("other.txt"), "other\n").expect("write unrelated");
        git(&unrelated, &["add", "other.txt"]);
        git(&unrelated, &["commit", "-m", "unrelated"]);

        let runtime = OrbitRuntime::from_roots(&global_root, &repo_root.join(".orbit"))
            .expect("build runtime");
        Self {
            _root: root,
            runtime,
            repo_root,
            unrelated,
        }
    }
}

fn cwd_context(
    cwd: &Path,
    orbit_host: Option<std::sync::Arc<dyn orbit_tools::OrbitToolHost>>,
    workspace_root: Option<PathBuf>,
) -> ToolContext {
    ToolContext {
        cwd: Some(cwd.to_string_lossy().into_owned()),
        orbit_host,
        workspace_root,
        ..Default::default()
    }
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn git(current_dir: &Path, args: &[&str]) {
    let output = crate::git_test_support::command()
        .args(args)
        .current_dir(current_dir)
        .output()
        .unwrap_or_else(|error| panic!("spawn git {}: {error}", args.join(" ")));
    assert!(
        output.status.success(),
        "git {} failed in {}:\nstdout: {}\nstderr: {}",
        args.join(" "),
        current_dir.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_stdout(current_dir: &Path, args: &[&str]) -> String {
    let output = crate::git_test_support::command()
        .args(args)
        .current_dir(current_dir)
        .output()
        .unwrap_or_else(|error| panic!("spawn git {}: {error}", args.join(" ")));
    assert!(output.status.success(), "git {} failed", args.join(" "));
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}
