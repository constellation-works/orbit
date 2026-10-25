//! A credential/transport stub drives the public publisher without a network
//! service. Real Git performs every cache operation and push to a local bare
//! repository; the stub gates transport on real `git credential fill`.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use chrono::Utc;
use orbit_common::{process, test_env};
use orbit_store::maintenance::task_registry::{
    BindWorkspaceParams, TaskRegistryStore, task_registry_path,
};
use orbit_store::workflow::task::{
    AttachmentPolicy, AttachmentPolicyKind, PublicationCallerRole, PublicationPublishRequest,
    PublicationPublishStatus, ScannerFailureBehavior, publish_task_snapshot,
};

const TEST: &str = "credentials::publication_askpass_authenticates_without_admitting_global_config";
const SECRET: &str = "fixture-password-never-log";

#[test]
fn publication_askpass_authenticates_without_admitting_global_config() {
    if std::env::var("ORBIT_CREDENTIAL_CHILD").as_deref() != Ok(TEST) {
        let root = tempfile::tempdir_in(test_env::canonical_temp_dir()).unwrap();
        let real_git = Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .unwrap();
        assert!(real_git.status.success());
        let mut child = Command::new(std::env::current_exe().unwrap());
        test_env::clear_inherited_authority(|key| {
            child.env_remove(key);
        });
        child
            .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
            .env("ORBIT_CREDENTIAL_CHILD", TEST)
            .env(
                "ORBIT_REAL_GIT",
                String::from_utf8(real_git.stdout).unwrap().trim(),
            )
            .env("HOME", root.path())
            .env("USERPROFILE", root.path())
            .env("GIT_CONFIG_GLOBAL", root.path().join(".gitconfig"))
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_ASKPASS", root.path().join("askpass"))
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_TRACE", root.path().join("git-trace"))
            .env("GIT_TRACE_CURL", root.path().join("curl-trace"))
            .env("GIT_TRACE2_EVENT", root.path().join("trace2"))
            .env("GIT_CURL_VERBOSE", "1")
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "core.hooksPath")
            .env("GIT_CONFIG_VALUE_0", root.path().join("hooks"))
            .env("GIT_CONFIG_PARAMETERS", "'core.symlinks'='true'")
            .env("ORBIT_CREDENTIAL_ROOT", root.path())
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    root.path().join("bin").display(),
                    std::env::var("PATH").unwrap()
                ),
            )
            .current_dir(root.path());
        let output = process::run_bounded_capped(&mut child, Duration::from_secs(60), 256 * 1024)
            .expect("credential fixture completes");
        test_env::assert_child_test_passed(TEST, output.status, output.stdout, output.stderr);
        return;
    }
    let root = std::env::current_dir().unwrap();
    fs::create_dir(root.join("bin")).unwrap();
    fs::create_dir(root.join("hooks")).unwrap();
    executable(
        &root.join("hooks/pre-push"),
        "#!/bin/sh\ntouch \"$ORBIT_CREDENTIAL_ROOT/hook-ran\"\nexit 1\n",
    );
    executable(
        &root.join("global-helper"),
        &format!(
            "#!/bin/sh\nprintf called > \"$ORBIT_CREDENTIAL_ROOT/global-helper-ran\"\nprintf 'username=fixture\\npassword={SECRET}\\n'\n"
        ),
    );
    fs::write(root.join(".gitconfig"), format!(
        "[credential \"https://auth.invalid\"]\n helper = {}\n[core]\n hooksPath = {}\n excludesFile = {}\n attributesFile = {}\n symlinks = true\n",
        root.join("global-helper").display(), root.join("hooks").display(),
        root.join("excludes").display(), root.join("attributes").display()
    )).unwrap();
    fs::write(root.join("excludes"), "*\n").unwrap();
    fs::write(root.join("attributes"), "* filter=host\n").unwrap();
    executable(&root.join("askpass"), "#!/bin/sh\nexit 1\n");
    fs::write(root.join("password"), SECRET).unwrap();
    executable(
        &root.join("bin/git"),
        r#"#!/bin/bash
set -eu
printf '%s\n' "$@" >> "$ORBIT_CREDENTIAL_ROOT/argv"
prefix=()
transport=false
for arg in "$@"; do
    case "$arg" in ls-remote|fetch|push) transport=true; break ;; esac
    prefix+=("$arg")
done
if "$transport"; then
    test "${GIT_TERMINAL_PROMPT:-}" = 0
    test "${GCM_INTERACTIVE:-}" = never
    test "${GIT_CONFIG_GLOBAL:-}" = /dev/null
    test "${GIT_CONFIG_SYSTEM:-}" = /dev/null
    test -z "${GIT_CONFIG_COUNT:-}"
    test -z "${GIT_CONFIG_PARAMETERS:-}"
    for setting in hooksPath excludesFile attributesFile; do
        test "$("$ORBIT_REAL_GIT" "${prefix[@]}" config --get "core.$setting")" = /dev/null
    done
    test "$("$ORBIT_REAL_GIT" "${prefix[@]}" config --get core.symlinks)" = false
    credentials=$(printf 'protocol=https\nhost=auth.invalid\n\n' | "$ORBIT_REAL_GIT" "${prefix[@]}" credential fill)
    password=$(printf '%s\n' "$credentials" | sed -n 's/^password=//p')
    test "$password" = "$(cat "$ORBIT_CREDENTIAL_ROOT/password")"
    if test -f "$ORBIT_CREDENTIAL_ROOT/refuse" && test "$arg" = "$(cat "$ORBIT_CREDENTIAL_ROOT/refuse")"; then
        printf 'helper diagnostic: %s https://auth.invalid/private.git\n' "$password" >&2
        exit 1
    fi
    exec "$ORBIT_REAL_GIT" -c "url.$ORBIT_CREDENTIAL_ROOT/remote.git.insteadOf=https://auth.invalid/private.git" "$@"
fi
exec "$ORBIT_REAL_GIT" "$@"
"#,
    );
    let real_git = std::env::var("ORBIT_REAL_GIT").unwrap();
    let init = Command::new(&real_git)
        .args(["init", "--bare", "--quiet", "remote.git"])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env_remove("GIT_CONFIG_COUNT")
        .env_remove("GIT_CONFIG_PARAMETERS")
        .env_remove("GIT_TRACE")
        .env_remove("GIT_TRACE_CURL")
        .env_remove("GIT_TRACE2_EVENT")
        .env_remove("GIT_CURL_VERBOSE")
        .status()
        .unwrap();
    assert!(init.success());
    let mut ordinary_git = Command::new("sh");
    ordinary_git
        .args(["-c", "printf 'protocol=https\\nhost=auth.invalid\\n\\n' | \"$ORBIT_REAL_GIT\" credential fill"])
        .env_remove("GIT_CONFIG_COUNT")
        .env_remove("GIT_CONFIG_PARAMETERS")
        .env_remove("GIT_TRACE")
        .env_remove("GIT_TRACE_CURL")
        .env_remove("GIT_TRACE2_EVENT")
        .env_remove("GIT_CURL_VERBOSE");
    let ordinary =
        process::run_bounded_capped(&mut ordinary_git, Duration::from_secs(10), 4096).unwrap();
    assert!(
        ordinary.status.success(),
        "ordinary Git must authenticate with the global helper"
    );
    assert!(String::from_utf8(ordinary.stdout).unwrap().contains(SECRET));
    assert!(root.join("global-helper-ran").is_file());
    fs::remove_file(root.join("global-helper-ran")).unwrap();

    let registry = TaskRegistryStore::open(&task_registry_path(&root.join("global"))).unwrap();
    let checkout = root.join("checkout");
    fs::create_dir_all(checkout.join(".orbit")).unwrap();
    registry
        .bind_workspace(BindWorkspaceParams {
            partition_id: Some("ws_credentials".into()),
            slug: "credentials".into(),
            repo_root: checkout.clone(),
            workspace_path: checkout.clone(),
            orbit_dir: checkout.join(".orbit"),
            repo_fingerprint: Some("ssh://source.invalid/orbit.git".into()),
        })
        .unwrap();
    let request = PublicationPublishRequest {
        workspace_id: "ws_credentials".into(),
        task_workspace_id: "ws_credentials".into(),
        source_repository_fingerprint: "ssh://source.invalid/orbit.git".into(),
        publication_id: "pub_credentials".into(),
        authority_machine_id: "hm_credentials".into(),
        local_machine_id: "hm_credentials".into(),
        caller_role: PublicationCallerRole::Owner,
        publication_remote: "https://auth.invalid/private.git".into(),
        publication_branch: "main".into(),
        cache_dir: root.join("cache"),
        published_at: Utc::now(),
        last_success: None,
    };
    let policy = AttachmentPolicy {
        kind: AttachmentPolicyKind::Omit,
        max_file_bytes: 1024,
        max_total_bytes: 1024,
        deny_patterns: Vec::new(),
        scanner_failure_behavior: ScannerFailureBehavior::Reject,
    };
    let publish = || publish_task_snapshot(&registry, request.clone(), &policy, None);
    let before = publish().expect_err("global-helper-only publication must fail");
    assert!(before.to_string().contains("failed"));
    assert!(
        !root.join("global-helper-ran").exists(),
        "publication must ignore the global helper"
    );
    // The supported bridge reads the credential at invocation time. Neither
    // its script nor any child argv contains the credential.
    executable(
        &root.join("askpass"),
        r#"#!/bin/sh
case "$1" in
  *Username*) printf '%s\n' fixture ;;
  *Password*) cat "$ORBIT_CREDENTIAL_ROOT/password" ;;
  *) exit 1 ;;
esac
"#,
    );
    let published = publish().expect("explicit askpass must authenticate unattended");
    assert_eq!(published.status, PublicationPublishStatus::Initialized);
    let mut acknowledged = request.clone();
    acknowledged.last_success = Some(orbit_store::workflow::task::PublicationLastSuccess {
        generation: published.generation,
        commit: published.commit_id,
    });
    let publish = || publish_task_snapshot(&registry, acknowledged.clone(), &policy, None);
    orbit_store::compose::workspace_task_backends(registry.clone(), "ws_credentials".into())
        .task
        .create_task(crate::support::create_params(
            "changed snapshot",
            orbit_types::task::TaskStatus::Backlog,
        ))
        .unwrap();
    let tracing = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let trace_output = tracing.clone();
    let _subscriber = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_writer(move || TraceWriter(trace_output.clone()))
            .finish(),
    );
    for command in ["ls-remote", "push"] {
        fs::write(root.join("refuse"), command).unwrap();
        let error = publish()
            .expect_err("transport refusal must be safe to report")
            .to_string();
        assert!(
            !error.contains(SECRET),
            "credential diagnostic leaked: {error}"
        );
        assert!(error.contains("transport diagnostics withheld"), "{error}");
        if command == "ls-remote" {
            assert!(
                !error.contains("https://auth.invalid/private.git"),
                "raw remote leaked: {error}"
            );
        }
    }
    assert!(
        !fs::read_to_string(root.join("argv"))
            .unwrap()
            .contains(SECRET)
    );
    assert!(
        !String::from_utf8(tracing.lock().unwrap().clone())
            .unwrap()
            .contains(SECRET)
    );
    for path in [
        "hook-ran",
        "global-helper-ran",
        "git-trace",
        "curl-trace",
        "trace2",
    ] {
        assert!(!root.join(path).exists(), "publication must isolate {path}");
    }
}

struct TraceWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
impl std::io::Write for TraceWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn executable(path: &Path, content: &str) {
    fs::write(path, content).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}
