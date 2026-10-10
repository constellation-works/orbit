use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::{
    PublicationInspectRequest, PublicationRestoreMode, PublicationRestoreRequest,
    inspect_publication, open_registry, restore_publication,
};

/// Bytes that exist only in the host file a publication symlink names.
const SENTINEL: &str = "ORBIT_PUBLICATION_LINK_TARGET_BYTES";

#[derive(Clone, Copy)]
enum PlantedLink {
    Envelope,
    Description,
}

impl PlantedLink {
    fn rel(self) -> &'static str {
        match self {
            Self::Envelope => "orbit-task-publication.yaml",
            Self::Description => "tasks/ORB-00001/description.md",
        }
    }
}

struct Fixture {
    _root: tempfile::TempDir,
    remote: PathBuf,
    cache: PathBuf,
    global: PathBuf,
    secret: PathBuf,
    planted: PlantedLink,
}

impl Fixture {
    fn plant(planted: PlantedLink) -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        let remote = root.path().join("publication");
        let cache = root.path().join("cache");
        let global = root.path().join("global");
        let secret = root.path().join("host-secret.txt");
        fs::create_dir_all(&remote).expect("publication repo");
        fs::create_dir_all(&cache).expect("cache");
        fs::create_dir_all(&global).expect("global");
        fs::write(&secret, SENTINEL).expect("host secret");
        git(&remote, &["init", "-b", "main"]);
        let ordinary = match planted {
            PlantedLink::Envelope => "tasks/ORB-00001/description.md",
            PlantedLink::Description => "orbit-task-publication.yaml",
        };
        write_regular(&remote, ordinary, "ordinary publication bytes\n");
        write_symlink(&remote, planted.rel(), &secret);
        git(
            &remote,
            &[
                "-c",
                "user.name=orbit-test",
                "-c",
                "user.email=orbit-test@example.com",
                "commit",
                "-m",
                "snapshot",
            ],
        );
        let listing = git_stdout(&remote, &["ls-tree", "-r", "HEAD"]);
        assert!(
            listing.contains("120000") && listing.contains(planted.rel()),
            "fixture must record {} as a symlink, got {listing}",
            planted.rel()
        );
        Self {
            _root: root,
            remote,
            cache,
            global,
            secret,
            planted,
        }
    }

    fn request(&self) -> PublicationInspectRequest {
        PublicationInspectRequest {
            workspace_id: "ws_symlink".to_string(),
            source_repository_fingerprint: "ssh://source.test/orbit.git".to_string(),
            publication_id: "pub_symlink".to_string(),
            authority_machine_id: "hm_symlink".to_string(),
            publication_remote: self.remote.to_str().expect("remote path").to_string(),
            publication_branch: "main".to_string(),
            cache_dir: self.cache.clone(),
            commit: None,
        }
    }

    fn restore_request(&self) -> PublicationRestoreRequest {
        PublicationRestoreRequest {
            task_workspace_id: "ws_symlink".to_string(),
            publication: self.request(),
            mode: PublicationRestoreMode::EmptyDestination,
        }
    }

    fn assert_refusal(&self, error: &impl std::fmt::Display) {
        let message = error.to_string();
        assert!(
            message.contains("symlink") && message.contains(self.planted.rel()),
            "ORB-14124: publication consume must refuse {} before reading it, got {message}",
            self.planted.rel()
        );
        assert!(
            !message.contains(SENTINEL),
            "ORB-14124: link target bytes reached the error: {message}"
        );
        let target = self.secret.display().to_string();
        assert!(
            !message.contains(&target),
            "ORB-14124: link target path reached the error: {message}"
        );
    }

    fn assert_sentinel_stayed_on_the_host(&self) {
        assert_tree_excludes(&self.cache, SENTINEL);
        assert_tree_excludes(&self.global, SENTINEL);
    }
}

/// Inspect and restore share one consumer. A symlink at the envelope or at a
/// task body must fail before that link is read, and the host file it names
/// must not show up in the error or the canonical store.
#[test]
fn publication_consume_refuses_symlink_before_reading_the_target() {
    for planted in [PlantedLink::Envelope, PlantedLink::Description] {
        let fixture = Fixture::plant(planted);
        let inspected = inspect_publication(fixture.request())
            .expect_err("inspect must refuse a publication symlink");
        fixture.assert_refusal(&inspected);
        fixture.assert_sentinel_stayed_on_the_host();

        let registry = open_registry(&fixture.global);
        let restored = restore_publication(&registry, fixture.restore_request())
            .expect_err("restore must refuse a publication symlink");
        fixture.assert_refusal(&restored);
        fixture.assert_sentinel_stayed_on_the_host();
    }
}

fn write_regular(repo: &Path, rel: &str, body: &str) {
    let path = repo.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("parent");
    }
    fs::write(&path, body).expect("regular file");
    git(repo, &["add", "--", rel]);
}

fn write_symlink(repo: &Path, rel: &str, target: &Path) {
    let target = target.to_str().expect("utf-8 link target");
    let hash = git_stdin(repo, &["hash-object", "-w", "--stdin"], target.as_bytes());
    let info = format!("120000,{hash},{rel}");
    git(repo, &["update-index", "--add", "--cacheinfo", &info]);
}

fn git(repo: &Path, args: &[&str]) {
    let output = git_output(repo, args, None);
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_stdout(repo: &Path, args: &[&str]) -> String {
    let output = git_output(repo, args, None);
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("git stdout")
}

fn git_stdin(repo: &Path, args: &[&str], stdin: &[u8]) -> String {
    let output = git_output(repo, args, Some(stdin));
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git stdout")
        .trim()
        .to_string()
}

fn git_output(repo: &Path, args: &[&str], stdin: Option<&[u8]>) -> std::process::Output {
    let mut command = Command::new("git");
    command
        .current_dir(repo)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "orbit-test")
        .env("GIT_AUTHOR_EMAIL", "orbit-test@example.com")
        .env("GIT_COMMITTER_NAME", "orbit-test")
        .env("GIT_COMMITTER_EMAIL", "orbit-test@example.com")
        .env_remove("GIT_CONFIG_COUNT")
        .env_remove("GIT_CONFIG_PARAMETERS");
    if stdin.is_some() {
        command.stdin(std::process::Stdio::piped());
    }
    let mut child = command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn git");
    if let Some(bytes) = stdin {
        use std::io::Write;
        child
            .stdin
            .as_mut()
            .expect("stdin")
            .write_all(bytes)
            .expect("write git stdin");
    }
    child.wait_with_output().expect("wait git")
}

fn assert_tree_excludes(root: &Path, sentinel: &str) {
    if !root.exists() {
        return;
    }
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let entries = fs::read_dir(&dir).unwrap_or_else(|error| {
            panic!("read {}: {error}", dir.display());
        });
        for entry in entries {
            let entry = entry.expect("dir entry");
            let file_type = entry.file_type().expect("file type");
            if file_type.is_symlink() {
                panic!(
                    "ORB-14124: publication consume left a symlink at {}",
                    entry.path().display()
                );
            }
            if file_type.is_dir() {
                pending.push(entry.path());
            } else if file_type.is_file() {
                let bytes = fs::read(entry.path()).expect("read file");
                let text = String::from_utf8_lossy(&bytes);
                assert!(
                    !text.contains(sentinel),
                    "ORB-14124: link target bytes reached {}",
                    entry.path().display()
                );
            }
        }
    }
}
