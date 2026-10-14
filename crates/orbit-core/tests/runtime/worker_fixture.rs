//! Detached substitute workers wait in this process, with a deadline, rather
//! than forking a shell's sleep command on every marker check.

use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

const WAIT_TEST: &str = "worker_fixture::wait_for_release";
const PATH_ENV: &str = "ORBIT_TEST_WORKER_WAIT_PATH";
const MODE_ENV: &str = "ORBIT_TEST_WORKER_WAIT_MODE";
const CONTROL_ENV: &str = "ORBIT_TEST_WORKER_WAIT_CONTROL";
const RUN_ENV: &str = "ORBIT_TEST_WORKER_WAIT_RUN";

pub(super) fn install(path: &Path, mode: &str) {
    install_with_control(path, mode, "");
}

/// Gate marker observation until the audit fixture releases its owned worker.
/// Other callers retain the original three modes and their polling contracts.
#[cfg(unix)]
pub(super) fn install_owned(path: &Path) -> std::net::TcpListener {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    install_with_control(path, "created", &listener.local_addr().unwrap().to_string());
    listener
}

fn install_with_control(path: &Path, mode: &str, control: &str) {
    orbit_common::test_env::assert_child_test_exists(WAIT_TEST);
    orbit_core::test_support::install_substitute_pipeline_worker([
        "env".to_string(),
        format!("{PATH_ENV}={}", path.display()),
        format!("{MODE_ENV}={mode}"),
        format!("{CONTROL_ENV}={control}"),
        format!("{RUN_ENV}={}", orbit_core::test_support::RUN_ID_PLACEHOLDER),
        std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
        "--exact".to_string(),
        WAIT_TEST.to_string(),
        "--ignored".to_string(),
        "--nocapture".to_string(),
    ]);
}

#[test]
#[ignore = "entry point for detached substitute workers"]
fn wait_for_release() {
    let path = std::env::var_os(PATH_ENV).expect("substitute worker wait path");
    let path = std::path::PathBuf::from(path);
    let mode = std::env::var(MODE_ENV).unwrap();
    let run = std::env::var(RUN_ENV).unwrap();
    let lifetime = if mode == "removed" { 60 } else { 120 };
    let deadline = Instant::now() + Duration::from_secs(lifetime);
    let control = std::env::var(CONTROL_ENV).unwrap_or_default();
    let owned = !control.is_empty();
    if owned {
        use std::io::Read;
        let mut control = std::net::TcpStream::connect_timeout(
            &control.parse().unwrap(),
            deadline.saturating_duration_since(Instant::now()),
        )
        .unwrap();
        control
            .set_read_timeout(Some(deadline.saturating_duration_since(Instant::now())))
            .unwrap();
        control
            .set_write_timeout(Some(deadline.saturating_duration_since(Instant::now())))
            .unwrap();
        // The private fixture listener learns the actual substitute's identity,
        // not the in-process executor PID persisted on the child run.
        writeln!(control, "{run} {}", std::process::id()).unwrap();
        let mut release = [0];
        control.read_exact(&mut release).unwrap();
        assert_eq!(release, [1], "owned fixture release");
    }
    loop {
        let released = match mode.as_str() {
            "removed" => !path.exists(),
            "created" => path.exists(),
            "drain" => {
                if path.join("leaves-exit").exists() && !path.join(format!("drain-{run}")).exists()
                {
                    // A leaf exits before claiming; the drain stays alive.
                    std::process::exit(3);
                }
                path.join(format!("started-{run}")).exists()
            }
            _ => panic!("unknown fixture wait mode: {mode}"),
        };
        if released {
            if owned {
                let _ = writeln!(
                    std::io::stderr(),
                    "worker release observed: run={run} pid={} exit=0",
                    std::process::id()
                );
            }
            return;
        }
        assert!(
            Instant::now() < deadline,
            "substitute worker release deadline"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}
