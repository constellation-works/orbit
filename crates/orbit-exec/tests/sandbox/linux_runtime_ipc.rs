//! Mandatory Linux runtime IPC masks, exercised against real host sockets.
#![cfg(target_os = "linux")]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use orbit_common::OrbitError;
use orbit_exec::{
    LinuxBwrapMask, LinuxBwrapSpawnRequest, compile_linux_bwrap_argv,
    compile_linux_bwrap_argv_with_authority, probe_bwrap, spawn_under_linux_bwrap,
};
use orbit_types::policy::ResolvedFsProfile;

#[test]
fn runtime_sockets_are_hidden_from_workers_and_descendants() {
    const TEST: &str = "linux_runtime_ipc::runtime_sockets_are_hidden_from_workers_and_descendants";
    const ROOT_ENV: &str = "ORBIT_RUNTIME_IPC_FIXTURE";
    let Some(root) = std::env::var_os(ROOT_ENV) else {
        // Unix socket paths are length limited. A short fixture root is bound
        // back over Bubblewrap's private /tmp by the policy below, so absence
        // of these sockets cannot be attributed to /tmp isolation.
        let root = tempfile::tempdir_in("/tmp").expect("runtime fixture");
        let mut command = Command::new(std::env::current_exe().expect("test binary"));
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", root.path().join("home"))
            .env("XDG_RUNTIME_DIR", root.path().join("runtime"))
            .env(ROOT_ENV, root.path())
            .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
            .stdin(Stdio::null());
        let output = orbit_common::process::run_bounded_capped(
            &mut command,
            std::time::Duration::from_secs(30),
            1024 * 1024,
        )
        .expect("isolated runtime fixture");
        orbit_common::test_env::assert_child_test_passed(
            TEST,
            output.status,
            output.stdout,
            output.stderr,
        );
        return;
    };
    let root = PathBuf::from(root).canonicalize().expect("fixture root");
    let runtime = root.join("runtime");
    std::fs::create_dir_all(runtime.join("systemd")).expect("systemd directory");
    std::fs::create_dir_all(runtime.join("gnupg")).expect("GPG directory");
    let socket_paths: Vec<_> = [
        "bus",
        "systemd/private",
        "openssh_agent",
        "gnupg/S.gpg-agent",
        "gnupg/S.gpg-agent.extra",
    ]
    .iter()
    .map(|path| runtime.join(path))
    .collect();
    let control = root.join("control");
    let _listeners: Vec<_> = socket_paths
        .iter()
        .chain([&control])
        .map(|path| {
            let listener = UnixListener::bind(path).expect("host socket");
            // Confirm each endpoint is live before applying any confinement.
            drop(UnixStream::connect(path).expect("host can connect"));
            listener
        })
        .collect();
    let profile = ResolvedFsProfile {
        name: "runtime-ipc".into(),
        read: vec!["/**".into()],
        modify: vec![format!("{}/**", root.display())],
    };
    let script = r#"
import errno, os, socket, subprocess, sys
runtime, control = sys.argv[1:]
paths = ['bus', 'systemd/private', 'openssh_agent',
         'gnupg/S.gpg-agent', 'gnupg/S.gpg-agent.extra']
def denied(path):
    with socket.socket(socket.AF_UNIX) as s:
        try:
            s.connect(path)
        except OSError as e:
            assert e.errno in (errno.ENOENT, errno.EACCES), (path, e)
        else:
            raise AssertionError('host socket reachable: ' + path)
for base in [runtime, '/run/user/' + str(os.getuid())]:
    for path in paths:
        denied(base + '/' + path)
with socket.socket(socket.AF_UNIX) as s:
    s.connect(control)
# proc.spawn children inherit this same boundary without another wrapper.
descendant = '''
import errno, socket, sys
with socket.socket(socket.AF_UNIX) as s:
    s.connect(sys.argv[2])
with socket.socket(socket.AF_UNIX) as s:
    try:
        s.connect(sys.argv[1])
    except OSError as e:
        assert e.errno in (errno.ENOENT, errno.EACCES), e
    else:
        raise AssertionError('descendant reached the host session bus')
'''
subprocess.run([sys.executable, '-I', '-c', descendant, runtime + '/bus', control],
    stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True, timeout=5)
# Query the manager rather than start a host unit. Even an explicitly restored
# bus address must fail before any systemd method can be reached.
if os.path.exists('/usr/bin/busctl'):
    for base in [runtime, '/run/user/' + str(os.getuid())]:
        p = subprocess.run(['/usr/bin/busctl', '--address=unix:path=' + base + '/bus',
            '--timeout=2', 'get-property', 'org.freedesktop.systemd1',
            '/org/freedesktop/systemd1', 'org.freedesktop.systemd1.Manager', 'SystemState'],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=5)
        assert p.returncode != 0, p.stdout
print('runtime sockets hidden; control connected; descendant and manager query refused')
"#;
    let args = vec![
        "-I".into(),
        "-c".into(),
        script.into(),
        runtime.display().to_string(),
        control.display().to_string(),
    ];
    let plan = compile_linux_bwrap_argv(&profile, "/usr/bin/python3", &args, Some(&root), false)
        .expect("runtime mask plan");
    assert!(
        plan.args
            .windows(2)
            .any(|pair| pair[0] == "--tmpfs" && pair[1] == runtime.display().to_string()),
        "an XDG runtime outside /run/user must be masked"
    );
    // The launcher must mask the well-known path independently of the env.
    // SAFETY: getuid has no arguments or memory-safety preconditions.
    let standard = PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() }));
    if standard.exists() {
        let standard = standard.canonicalize().expect("standard runtime");
        assert!(
            plan.args
                .windows(2)
                .any(|pair| pair[0] == "--tmpfs" && pair[1] == standard.display().to_string())
        );
    }
    for grant in [&runtime, &runtime.join("systemd")] {
        let granted = ResolvedFsProfile {
            modify: vec![format!("{}/**", grant.display())],
            ..profile.clone()
        };
        assert!(
            matches!(
                compile_linux_bwrap_argv(&granted, "/bin/true", &[], None, false),
                Err(OrbitError::PolicyDenied(_))
            ),
            "a policy grant cannot reopen runtime sockets"
        );
    }
    let sentinel = runtime.join("systemd");
    let target = root.join("masked");
    std::fs::create_dir(&target).expect("caller mask target");
    assert!(
        matches!(
            compile_linux_bwrap_argv_with_authority(
                &profile,
                "/bin/true",
                &[],
                Some(&root),
                false,
                Vec::new(),
                Some(&LinuxBwrapMask {
                    sentinel,
                    targets: vec![target],
                    files: Vec::new()
                }),
            ),
            Err(OrbitError::PolicyDenied(_))
        ),
        "a caller mask sentinel cannot alias the host runtime"
    );

    let probe = probe_bwrap();
    if !probe.available {
        orbit_exec::report_bwrap_deferral(TEST, &probe.detail);
        return;
    }
    let child = spawn_under_linux_bwrap(LinuxBwrapSpawnRequest {
        plan: &plan,
        env: &[("PATH".into(), "/usr/bin:/bin".into())],
        cwd: Some(&root),
        stdin: Stdio::null(),
        stdout: Stdio::piped(),
        stderr: Stdio::piped(),
    })
    .expect("sandbox child");
    let output = child.wait_with_output().expect("wait for socket checks");
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
