//! The supervised dashboard server through its process boundary.
//!
//! This binary is its own fake `orbit`: run as `web serve …` it serves a
//! loopback page and, on `GET /handover`, replaces its image the way the real
//! server does after an update, replaying its arguments (so `--port 0`) in the
//! same PID and announcing the new address. It has no libtest harness, so the
//! supervisor's `web serve` arguments reach `main` intact.
//!
//! A second scenario quits the app while a server has started but is not yet
//! tracked, holding the supervisor at that point through [`Shell::spawned`].
//! Draining scenarios hold a child after SIGTERM to check overlapping quits
//! and the deadline for a server that never finishes draining.

#[cfg(unix)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.starts_with(&["web".into(), "serve".into()]) {
        return fake::serve();
    }
    handover::follows_a_new_address_in_the_same_process()?;
    starting::quitting_while_a_server_starts_stops_it()?;
    draining::overlapping_shutdowns_wait_for_reaping()?;
    draining::unresponsive_server_is_killed()
}

#[cfg(not(unix))]
fn main() {}

#[cfg(unix)]
mod handover {
    use std::error::Error;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex, PoisonError};
    use std::time::{Duration, Instant};

    use orbit_desktop::sidecar::{self, Shell, Sidecar};
    use url::Url;

    use crate::fake;

    /// A window that records where it was sent, launching this binary as
    /// `orbit`.
    struct Recorder(Arc<Mutex<Vec<Url>>>);

    impl Shell for Recorder {
        fn navigate(&self, url: Url) {
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(url);
        }

        fn eval(&self, _script: &str) {}

        fn log_path(&self) -> Option<PathBuf> {
            None
        }

        fn orbit(&self) -> Result<PathBuf, String> {
            std::env::current_exe().map_err(|error| error.to_string())
        }
    }

    pub(crate) fn follows_a_new_address_in_the_same_process() -> Result<(), Box<dyn Error>> {
        let navigations = Arc::new(Mutex::new(Vec::new()));
        let (sidecar, control) = Sidecar::new();
        let supervisor = {
            let shell = Recorder(Arc::clone(&navigations));
            let sidecar = Arc::clone(&sidecar);
            std::thread::spawn(move || sidecar::supervise(shell, sidecar, control))
        };
        let outcome = scenario(&sidecar, &navigations);
        sidecar.shutdown();
        supervisor
            .join()
            .map_err(|_| "the supervisor thread panicked")?;
        let pid = outcome?;
        ensure(
            !process_exists(pid),
            format!("shutdown left the server process {pid} running or unreaped"),
        )
    }

    /// Drive a handover and return the server's PID.
    fn scenario(sidecar: &Sidecar, navigations: &Mutex<Vec<Url>>) -> Result<i32, Box<dyn Error>> {
        let first = navigation(navigations, 0)?;
        ensure(
            sidecar.allows_navigation(&first),
            format!("the first dashboard {first} is not an allowed origin"),
        )?;
        let pid = fake::pid(&first, "/")?;

        fake::pid(&first, "/handover")?;
        let second = navigation(navigations, 1)?;
        ensure(
            second.origin() != first.origin(),
            format!("the replacement announced the old address {first} again"),
        )?;
        ensure(
            fake::pid(&second, "/")? == pid,
            "the replacement dashboard is served by another process",
        )?;
        ensure(
            fake::pid(&first, "/").is_err(),
            format!("the old address {first} still serves after the handover"),
        )?;
        ensure(
            sidecar.allows_navigation(&second),
            format!("the window may not load the new dashboard {second}"),
        )?;
        ensure(
            !sidecar.allows_navigation(&first),
            format!("the window still allows the old dashboard {first}"),
        )?;
        ensure(
            sidecar.status()["phase"] == "ready",
            format!(
                "a live handover was treated as a stop: {}",
                sidecar.status()
            ),
        )?;
        Ok(pid)
    }

    /// The window's navigation number `index`, waiting for it.
    fn navigation(navigations: &Mutex<Vec<Url>>, index: usize) -> Result<Url, Box<dyn Error>> {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let seen = navigations
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            if let Some(url) = seen.get(index) {
                return Ok(url.clone());
            }
            if Instant::now() > deadline {
                return Err(format!(
                    "the window was never sent to dashboard address {} (navigations: {seen:?})",
                    index + 1
                )
                .into());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Whether `pid` names a process, including an unreaped one.
    pub(crate) fn process_exists(pid: i32) -> bool {
        // SAFETY: signal 0 only checks that the process exists.
        unsafe { libc::kill(pid, 0) == 0 }
    }

    pub(crate) fn ensure(holds: bool, message: impl Into<String>) -> Result<(), Box<dyn Error>> {
        if holds {
            Ok(())
        } else {
            Err(message.into().into())
        }
    }
}

#[cfg(unix)]
mod starting {
    use std::error::Error;
    use std::path::PathBuf;
    use std::sync::mpsc::{self, Receiver, SyncSender};
    use std::sync::{Arc, Mutex, PoisonError};
    use std::thread::JoinHandle;
    use std::time::Duration;

    use orbit_desktop::sidecar::{self, Shell, Sidecar};
    use url::Url;

    use crate::handover::{ensure, process_exists};

    /// How long the supervisor is held after the app starts quitting. A
    /// shutdown that does not wait for the starting server returns well
    /// within it, before the server is tracked.
    const HOLD: Duration = Duration::from_millis(500);

    /// The app quitting: the server's PID, and whether it was gone when
    /// `shutdown` returned.
    type Quit = (i32, JoinHandle<bool>);

    /// A window that quits the app the moment a server has started, launching
    /// this binary as `orbit`.
    struct QuitOnSpawn {
        sidecar: Arc<Sidecar>,
        quits: Mutex<Option<SyncSender<Quit>>>,
    }

    impl Shell for QuitOnSpawn {
        fn navigate(&self, _url: Url) {}

        fn eval(&self, _script: &str) {}

        fn log_path(&self) -> Option<PathBuf> {
            None
        }

        fn orbit(&self) -> Result<PathBuf, String> {
            std::env::current_exe().map_err(|error| error.to_string())
        }

        fn spawned(&self, pid: u32) {
            let Some(quits) = self
                .quits
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
            else {
                return;
            };
            let Ok(pid) = i32::try_from(pid) else {
                return;
            };
            let (returned_tx, returned) = mpsc::sync_channel(1);
            let sidecar = Arc::clone(&self.sidecar);
            // Quit from another thread, as the app's exit handler does, and
            // hold the supervisor here until that shutdown returns or `HOLD`
            // passes.
            let quit = std::thread::spawn(move || {
                sidecar.shutdown();
                let gone = !process_exists(pid);
                let _ = returned_tx.send(());
                gone
            });
            let _ = returned.recv_timeout(HOLD);
            let _ = quits.send((pid, quit));
        }
    }

    pub(crate) fn quitting_while_a_server_starts_stops_it() -> Result<(), Box<dyn Error>> {
        let (sidecar, control) = Sidecar::new();
        let (quits_tx, quits) = mpsc::sync_channel(1);
        let supervisor = {
            let shell = QuitOnSpawn {
                sidecar: Arc::clone(&sidecar),
                quits: Mutex::new(Some(quits_tx)),
            };
            let sidecar = Arc::clone(&sidecar);
            std::thread::spawn(move || sidecar::supervise(shell, sidecar, control))
        };
        let outcome = check(&quits);
        // Stops a server the checked shutdown left behind, so a failing run
        // leaves no process.
        sidecar.shutdown();
        supervisor
            .join()
            .map_err(|_| "the supervisor thread panicked")?;
        outcome
    }

    fn check(quits: &Receiver<Quit>) -> Result<(), Box<dyn Error>> {
        let (pid, quit) = quits
            .recv_timeout(Duration::from_secs(30))
            .map_err(|_| "the supervisor never started a server")?;
        let gone = quit.join().map_err(|_| "the quitting thread panicked")?;
        ensure(
            gone,
            format!(
                "quitting while the server started returned with process {pid} still running or unreaped"
            ),
        )
    }
}

#[cfg(unix)]
mod draining {
    use std::error::Error;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpStream;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::mpsc::{self, SyncSender};
    use std::time::{Duration, Instant};

    use orbit_desktop::sidecar::{self, Shell, Sidecar};
    use url::Url;

    use crate::handover::{ensure, process_exists};

    struct Window(SyncSender<Url>);

    impl Shell for Window {
        fn navigate(&self, url: Url) {
            let _ = self.0.send(url);
        }

        fn eval(&self, _script: &str) {}

        fn log_path(&self) -> Option<PathBuf> {
            None
        }

        fn orbit(&self) -> Result<PathBuf, String> {
            std::env::current_exe().map_err(|error| error.to_string())
        }
    }

    pub(crate) fn overlapping_shutdowns_wait_for_reaping() -> Result<(), Box<dyn Error>> {
        scenario(true)
    }

    pub(crate) fn unresponsive_server_is_killed() -> Result<(), Box<dyn Error>> {
        scenario(false)
    }

    fn scenario(release: bool) -> Result<(), Box<dyn Error>> {
        let (sidecar, control) = Sidecar::new();
        let (navigation_tx, navigation) = mpsc::sync_channel(1);
        let supervisor = {
            let sidecar = Arc::clone(&sidecar);
            std::thread::spawn(move || sidecar::supervise(Window(navigation_tx), sidecar, control))
        };
        let outcome = (|| {
            let url = navigation.recv_timeout(Duration::from_secs(30))?;
            let mut stream = TcpStream::connect((
                url.host_str().ok_or("dashboard URL has no host")?,
                url.port().ok_or("dashboard URL has no port")?,
            ))?;
            stream.set_read_timeout(Some(Duration::from_secs(20)))?;
            write!(stream, "GET /drain HTTP/1.1\r\nHost: localhost\r\n\r\n")?;
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            // This test-only endpoint uses a line protocol: PID once SIGTERM
            // is blocked, acknowledgement after receipt, then release byte.
            reader.read_line(&mut line)?;
            let pid: i32 = line.trim().parse()?;
            let (first_tx, first_returned) = mpsc::sync_channel(1);
            let first = {
                let sidecar = Arc::clone(&sidecar);
                std::thread::spawn(move || {
                    sidecar.shutdown();
                    let gone = !process_exists(pid);
                    let _ = first_tx.send(());
                    gone
                })
            };
            let started = Instant::now();
            let check = (|| {
                line.clear();
                reader.read_line(&mut line)?;
                ensure(line == "draining\n", "child never acknowledged SIGTERM")?;
                ensure(process_exists(pid), "held draining child exited early")?;
                ensure(
                    first_returned.try_recv().is_err(),
                    "first shutdown returned while its child was draining",
                )?;
                if !release {
                    first_returned.recv_timeout(Duration::from_secs(20))?;
                    return ensure(
                        started.elapsed() < Duration::from_secs(20),
                        "shutdown did not bound the unresponsive server's drain",
                    );
                }
                let (entered_tx, entered) = mpsc::sync_channel(1);
                let (returned_tx, returned) = mpsc::sync_channel(1);
                let second = {
                    let sidecar = Arc::clone(&sidecar);
                    std::thread::spawn(move || {
                        let _ = entered_tx.send(());
                        sidecar.shutdown();
                        let gone = !process_exists(pid);
                        let _ = returned_tx.send(());
                        gone
                    })
                };
                // Start the second caller only after the first one's SIGTERM
                // has reached the child. No scheduling load or startup race.
                let observation = (|| {
                    entered.recv_timeout(Duration::from_secs(5))?;
                    Ok::<_, Box<dyn Error>>(
                        returned.recv_timeout(Duration::from_millis(500)).is_ok(),
                    )
                })();
                // Release even when the assertion will fail, and join every
                // caller before reporting it so the red regression is clean.
                let released = reader.get_mut().write_all(b"x");
                let second_gone = second.join().map_err(|_| "second shutdown panicked")?;
                released?;
                ensure(
                    !observation?,
                    "second shutdown returned while the first caller's child was still draining",
                )?;
                ensure(
                    second_gone,
                    "second shutdown returned before the child was reaped",
                )
            })();
            // EOF also releases the child after a protocol error.
            drop(reader);
            let first_gone = first.join().map_err(|_| "first shutdown panicked")?;
            check?;
            ensure(
                first_gone,
                "first shutdown returned before the child was reaped",
            )?;
            ensure(
                !process_exists(pid),
                "draining child was left running or unreaped",
            )
        })();
        sidecar.shutdown();
        supervisor.join().map_err(|_| "supervisor panicked")?;
        outcome
    }
}

#[cfg(unix)]
mod fake {
    use std::error::Error;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::os::unix::process::CommandExt;
    use std::process::Command;
    use std::time::Duration;

    use url::Url;

    /// Port of the image this one replaced, so the replacement binds another.
    const PREVIOUS_PORT: &str = "ORBIT_DESKTOP_FAKE_PREVIOUS_PORT";

    /// Serve until stopped, replacing this image on `GET /handover`.
    pub(crate) fn serve() -> Result<(), Box<dyn Error>> {
        let previous = std::env::var(PREVIOUS_PORT).ok();
        let mut listener = TcpListener::bind("127.0.0.1:0")?;
        // An ephemeral port can come round again; hold it and take another.
        let _reused = if Some(listener.local_addr()?.port().to_string()) == previous {
            Some(std::mem::replace(
                &mut listener,
                TcpListener::bind("127.0.0.1:0")?,
            ))
        } else {
            None
        };
        let address = listener.local_addr()?;
        writeln!(std::io::stdout(), "Dashboard listening on http://{address}")?;
        for stream in listener.incoming() {
            let mut stream = stream?;
            let mut reader = BufReader::new(stream.try_clone()?);
            let mut request = String::new();
            reader.read_line(&mut request)?;
            let mut header = String::new();
            while reader.read_line(&mut header)? > 2 {
                header.clear();
            }
            if request.starts_with("GET /drain ") {
                return drain(stream);
            }
            let body = std::process::id().to_string();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )?;
            drop(stream);
            if request.starts_with("GET /handover ") {
                // Like the real server: exec the installed binary with the
                // original arguments. The listener closes on exec.
                let error = Command::new(std::env::current_exe()?)
                    .args(std::env::args_os().skip(1))
                    .env(PREVIOUS_PORT, address.port().to_string())
                    .exec();
                return Err(error.into());
            }
        }
        Ok(())
    }

    /// A deliberately held drain. This fake process is single-threaded, so
    /// blocking SIGTERM here ensures it is consumed only by `sigwait`.
    fn drain(mut stream: TcpStream) -> Result<(), Box<dyn Error>> {
        // SAFETY: initialize a local signal set, block it on this thread, and
        // wait with valid pointers. No signal handler or shared memory is used.
        let signals = unsafe {
            let mut signals: libc::sigset_t = std::mem::zeroed();
            if libc::sigemptyset(&mut signals) != 0
                || libc::sigaddset(&mut signals, libc::SIGTERM) != 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
            let error = libc::pthread_sigmask(libc::SIG_BLOCK, &signals, std::ptr::null_mut());
            if error != 0 {
                return Err(std::io::Error::from_raw_os_error(error).into());
            }
            signals
        };
        writeln!(stream, "{}", std::process::id())?;
        let mut signal = 0;
        // SAFETY: the set and output pointer are valid; SIGTERM is blocked.
        let error = unsafe { libc::sigwait(&signals, &mut signal) };
        if error != 0 {
            return Err(std::io::Error::from_raw_os_error(error).into());
        }
        writeln!(stream, "draining")?;
        let mut release = [0];
        if stream.read(&mut release)? == 1 && release[0] != b'x' {
            return Err("unexpected drain release byte".into());
        }
        Ok(())
    }

    /// The PID the fake dashboard at `url` reports for `path`.
    pub(crate) fn pid(url: &Url, path: &str) -> Result<i32, Box<dyn Error>> {
        let host = url.host_str().ok_or("dashboard URL has no host")?;
        let port = url.port().ok_or("dashboard URL has no port")?;
        let mut stream = TcpStream::connect((host, port))?;
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        write!(stream, "GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\n\r\n")?;
        let mut response = String::new();
        stream.read_to_string(&mut response)?;
        let (_, body) = response
            .split_once("\r\n\r\n")
            .ok_or("malformed fake dashboard response")?;
        Ok(body.trim().parse()?)
    }
}
