//! Deterministic negative controls for owned-server readiness.

use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::bound_server::{Announcement, BoundServer};
use orbit_common::test_env;

fn orbit(work: &Path, home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orbit"));
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(work)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("RUST_LOG", "warn,orbit::command::mcp::server=info")
        .env("NO_COLOR", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

#[test]
fn a_decoy_listener_cannot_make_a_failed_bind_ready() {
    let temp = tempfile::tempdir().expect("fixture directory");
    let home = temp.path().join("home");
    std::fs::create_dir_all(&home).expect("fixture home");
    let initialized = orbit(temp.path(), &home)
        .args([
            "init",
            "--non-interactive",
            "--machine-name",
            "readiness",
            "--task-prefix",
            "RDY",
        ])
        .output()
        .expect("initialize fixture");
    assert!(initialized.status.success(), "{initialized:?}");

    // Keep this socket owned throughout both real server launches. There is
    // no port churn and neither child can bind the decoy's address.
    let decoy = TcpListener::bind("127.0.0.1:0").expect("owned decoy");
    let address = decoy.local_addr().expect("decoy address");
    for (kind, args) in [
        (
            Announcement::Dashboard,
            vec![
                "web".into(),
                "serve".into(),
                "--port".into(),
                address.port().to_string(),
                "--no-open".into(),
            ],
        ),
        (
            Announcement::Mcp,
            vec!["mcp".into(), "listen".into(), address.to_string()],
        ),
    ] {
        let mut server = BoundServer::spawn(orbit(temp.path(), &home).args(args), kind)
            .expect("launch intended server");
        let error = server
            .wait_for_address(test_env::FIXTURE_STEP_DEADLINE)
            .expect_err("an unrelated listener must never establish child readiness");
        let diagnostic = error.to_string();
        assert!(
            diagnostic.contains("Address already in use")
                && diagnostic.contains(&address.to_string()),
            "must observe the intended child's bind failure: {diagnostic}"
        );
        assert!(
            !server
                .try_wait()
                .expect("poll failed server")
                .expect("server exited")
                .success()
        );
        assert!(
            TcpStream::connect_timeout(&address, Duration::from_secs(5)).is_ok(),
            "decoy remains connectable after the intended child exits"
        );
    }
}

#[cfg(unix)]
#[test]
fn a_reaped_childs_queued_announcement_cannot_make_a_decoy_ready() {
    let decoy = TcpListener::bind("127.0.0.1:0").expect("owned decoy");
    let address = decoy.local_addr().expect("decoy address");
    for (kind, line, script) in [
        (
            Announcement::Dashboard,
            format!("Dashboard listening on http://{address}"),
            "printf '%s\\n' \"$1\"; exit 41",
        ),
        (
            Announcement::Mcp,
            format!("INFO orbit::command::mcp::server: orbit mcp listener bound address={address}"),
            "printf '%s\\n' \"$1\" >&2; exit 41",
        ),
    ] {
        let mut server = BoundServer::spawn(
            Command::new("/bin/sh")
                .args(["-c", script, "readiness-fixture", &line])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
            kind,
        )
        .expect("launch announced-but-exited child");
        assert_eq!(
            server.wait().expect("deterministically reap child").code(),
            Some(41)
        );
        let error = server
            .wait_for_address(Duration::from_secs(5))
            .expect_err("queued announcements from a reaped child must be rejected");
        assert!(
            error.to_string().contains("server exited before readiness"),
            "{error}"
        );
        assert!(
            TcpStream::connect_timeout(&address, Duration::from_secs(5)).is_ok(),
            "decoy stays connectable throughout the negative control"
        );
    }
}

#[cfg(unix)]
mod inherited_pipes {
    use super::*;
    use std::io::{Read, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::sync::mpsc;
    use std::time::Instant;

    const SERVER: &str = "server_readiness::inherited_pipes::server_child";
    const HOLDER: &str = "server_readiness::inherited_pipes::pipe_holder_child";

    #[test]
    fn reaped_server_output_has_a_deadline_and_drop_cleans_its_descendant() {
        for kind in [Announcement::Dashboard, Announcement::Mcp] {
            let (_temp, mut server, mut holder) = fixture(kind);
            server.stdin.as_mut().unwrap().write_all(b"x").unwrap();
            assert_eq!(server.wait().unwrap().code(), Some(41));
            let started = Instant::now();
            let watchdog = Watchdog::new(&holder);
            let error = server
                .wait_for_address(Duration::from_millis(100))
                .unwrap_err();
            let rescued = watchdog.finish();
            assert!(
                !rescued,
                "reader needed the inherited writer to close: {error}"
            );
            assert!(started.elapsed() < Duration::from_secs(2), "{error}");
            let diagnostic = error.to_string();
            assert!(
                diagnostic.contains("server exited before readiness"),
                "{diagnostic}"
            );
            assert!(
                diagnostic.contains("exit status: 41"),
                "original exit status: {diagnostic}"
            );
            assert!(
                diagnostic.contains("original exit diagnostic"),
                "{diagnostic}"
            );
            // EOF has deliberately not happened: the descendant can still reply
            // after readiness finalized and joined both output reader threads.
            holder.write_all(b"p").unwrap();
            let mut reply = [0];
            holder.read_exact(&mut reply).unwrap();
            assert_eq!(&reply, b"p");
            assert_cleanup(server, holder, false);
        }
    }

    #[test]
    fn drop_and_assertion_unwind_finalize_inherited_pipes_without_readiness() {
        for unwind in [false, true] {
            let (_temp, mut server, holder) = fixture(Announcement::Dashboard);
            if !unwind {
                server.stdin.as_mut().unwrap().write_all(b"x").unwrap();
                assert_eq!(server.wait().unwrap().code(), Some(41));
            }
            assert_cleanup(server, holder, unwind);
        }
    }

    fn assert_cleanup(server: BoundServer, mut holder: UnixStream, unwind: bool) {
        let pid = server.id();
        let started = Instant::now();
        let watchdog = Watchdog::new(&holder);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _server = server;
            if unwind {
                panic!("exercise fixture assertion teardown");
            }
        }));
        assert!(
            !watchdog.finish(),
            "teardown needed a cooperative pipe release"
        );
        assert_eq!(result.is_err(), unwind);
        // The guard must reap the original parent on assertion unwind as well
        // as clean teardown; EOF alone could leave an unreaped zombie.
        // SAFETY: probe our just-owned PID without sending a signal.
        assert_eq!(unsafe { libc::kill(pid as libc::pid_t, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "teardown exceeded its two-second budget plus scheduling allowance"
        );
        let mut byte = [0];
        assert_eq!(
            holder.read(&mut byte).unwrap(),
            0,
            "fixture-owned holder survived teardown"
        );
    }

    // Rescue a broken implementation without leaving a blocked reader or helper
    // behind. Successful execution cancels this timer immediately; it never
    // waits for the watchdog's deadline or relies on host timing to order events.
    struct Watchdog {
        done: mpsc::Sender<()>,
        rescue: Option<std::thread::JoinHandle<bool>>,
    }

    impl Watchdog {
        fn new(holder: &UnixStream) -> Self {
            let (done, completion) = mpsc::channel();
            let mut release = holder.try_clone().unwrap();
            let rescue = std::thread::spawn(move || {
                if completion.recv_timeout(Duration::from_secs(3)).is_err() {
                    let _ = release.write_all(b"q");
                    true
                } else {
                    false
                }
            });
            Self {
                done,
                rescue: Some(rescue),
            }
        }

        fn finish(mut self) -> bool {
            self.done.send(()).unwrap();
            self.rescue.take().unwrap().join().unwrap()
        }
    }

    impl Drop for Watchdog {
        fn drop(&mut self) {
            // Join even if an assertion before finish unwinds the fixture.
            let _ = self.done.send(());
            if let Some(rescue) = self.rescue.take() {
                let _ = rescue.join();
            }
        }
    }

    fn fixture(kind: Announcement) -> (tempfile::TempDir, BoundServer, UnixStream) {
        test_env::assert_child_test_exists(SERVER);
        test_env::assert_child_test_exists(HOLDER);
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("holder.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut server = BoundServer::spawn(
            Command::new(std::env::current_exe().unwrap())
                .args(["--exact", SERVER, "--ignored", "--nocapture"])
                .env("READINESS_HOLDER_SOCKET", &socket)
                .env("READINESS_HOLDER_TEST", HOLDER)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
            kind,
        )
        .unwrap();
        let mut holder = None;
        test_env::wait_until("pipe holder control connection", || {
            assert!(
                server.try_wait().unwrap().is_none(),
                "server exited before holder handshake"
            );
            match listener.accept() {
                Ok((stream, _)) => {
                    holder = Some(stream);
                    true
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => false,
                Err(error) => panic!("holder connection: {error}"),
            }
        });
        let mut holder = holder.unwrap();
        // macOS accept inherits the listener's nonblocking flag. The control
        // socket uses bounded blocking reads after the connection rendezvous.
        holder.set_nonblocking(false).unwrap();
        holder
            .set_read_timeout(Some(test_env::FIXTURE_STEP_DEADLINE))
            .unwrap();
        holder
            .set_write_timeout(Some(test_env::FIXTURE_STEP_DEADLINE))
            .unwrap();
        let mut ready = [0];
        holder.read_exact(&mut ready).unwrap();
        assert_eq!(&ready, b"r", "holder owns both inherited pipes");
        (temp, server, holder)
    }

    #[test]
    #[ignore = "child entry point for the inherited-pipe negative control"]
    // The server must exit before its holder; the group guard owns cleanup.
    #[allow(clippy::zombie_processes)]
    fn server_child() {
        std::io::stdout()
            .write_all(b"Dashboard listening on http://127.0.0.1:12345\n")
            .unwrap();
        std::io::stderr()
            .write_all(
                b"orbit mcp listener bound address=127.0.0.1:12345\noriginal exit diagnostic\n",
            )
            .unwrap();
        let _holder = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                &std::env::var("READINESS_HOLDER_TEST").unwrap(),
                "--ignored",
                "--nocapture",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let mut release = [0];
        std::io::stdin().read_exact(&mut release).unwrap();
        std::process::exit(41);
    }

    #[test]
    #[ignore = "child entry point owning inherited output until cleanup"]
    fn pipe_holder_child() {
        // Exercise partial lines and oversized-line discard while the server
        // lives. This finite write exceeds pipe capacity and must be drained.
        std::io::stdout()
            .write_all(b"unterminated inherited stdout")
            .unwrap();
        std::io::stderr()
            .write_all(&vec![b'x'; 256 * 1024])
            .unwrap();
        let mut control =
            UnixStream::connect(std::env::var("READINESS_HOLDER_SOCKET").unwrap()).unwrap();
        control.write_all(b"r").unwrap();
        let mut command = [0];
        while control.read_exact(&mut command).is_ok() {
            if command == *b"q" {
                break;
            }
            control.write_all(&command).unwrap();
        }
    }
}
