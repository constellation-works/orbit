//! Owns the `orbit web serve` process the window shows.
//!
//! The app always runs its own dashboard server from the user's installed
//! `orbit`, so the UI, API and stores it touches are the same version the CLI
//! uses. It never attaches to whatever already listens on the conventional
//! port: on a shared host that could be another user's process, and a
//! dashboard started with a different `--root` would show the wrong registry.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tauri::{AppHandle, Manager, Url};

use crate::MAIN_WINDOW;

/// Printed by `orbit web serve` once its listener is bound.
const LISTENING_PREFIX: &str = "Dashboard listening on ";
/// How long a starting server has to report its address.
const READY_TIMEOUT: Duration = Duration::from_secs(60);
/// A server that ran this long before stopping restarts without backoff.
const HEALTHY_RUN: Duration = Duration::from_secs(60);
/// Upper bound on the delay between restarts of a failing server.
const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// Covers the server's own 10-second connection drain on SIGTERM.
const STOP_TIMEOUT: Duration = Duration::from_secs(12);
/// Poll interval for child exit and control messages.
const POLL: Duration = Duration::from_millis(250);
/// Room for a burst of retry clicks; extra ones are dropped.
const CONTROL_CAPACITY: usize = 8;
/// The log is truncated on launch past this size.
const MAX_LOG_BYTES: u64 = 8 * 1024 * 1024;

/// Requests from the window and the app lifecycle to the supervisor thread.
pub(crate) enum Control {
    /// Start again now instead of waiting out the backoff.
    Retry,
    /// Stop the running server and start a new one.
    Restart,
    /// The app is exiting.
    Shutdown,
}

/// State shared by the supervisor thread, the IPC commands and the window's
/// navigation policy.
pub(crate) struct Sidecar {
    status: Mutex<Value>,
    /// Origin of the running dashboard; the only remote origin the window
    /// may navigate to.
    dashboard: Mutex<Option<Url>>,
    /// Whether the window last started loading the status page.
    on_status_page: AtomicBool,
    child: Mutex<Option<Child>>,
    shutting_down: AtomicBool,
    control: SyncSender<Control>,
}

impl Sidecar {
    pub(crate) fn new() -> (Arc<Self>, Receiver<Control>) {
        let (control, receiver) = mpsc::sync_channel(CONTROL_CAPACITY);
        let sidecar = Arc::new(Self {
            status: Mutex::new(status_value(
                "starting",
                "Starting the Orbit dashboard…",
                None,
            )),
            dashboard: Mutex::new(None),
            on_status_page: AtomicBool::new(true),
            child: Mutex::new(None),
            shutting_down: AtomicBool::new(false),
            control,
        });
        (sidecar, receiver)
    }

    pub(crate) fn status(&self) -> Value {
        lock(&self.status).clone()
    }

    pub(crate) fn retry(&self) {
        let _ = self.control.try_send(Control::Retry);
    }

    pub(crate) fn restart(&self) {
        let _ = self.control.try_send(Control::Restart);
    }

    /// Address of the running dashboard server, if one is ready.
    pub(crate) fn dashboard(&self) -> Option<Url> {
        lock(&self.dashboard).clone()
    }

    /// Record which page the window is loading. The webview's own URL is
    /// unset until its first load, so it is never queried.
    pub(crate) fn page_loading(&self, url: &Url) {
        self.on_status_page
            .store(is_status_page(url), Ordering::SeqCst);
    }

    /// Whether the main window may load `url` in place.
    pub(crate) fn allows_navigation(&self, url: &Url) -> bool {
        url.scheme() == "about"
            || is_status_page(url)
            || lock(&self.dashboard)
                .as_ref()
                .is_some_and(|dashboard| dashboard.origin() == url.origin())
    }

    /// Stop the server without waiting for it to drain. For a panicking app,
    /// which may never reach its exit handler.
    pub(crate) fn kill_now(&self) {
        self.shutting_down.store(true, Ordering::SeqCst);
        if let Ok(mut slot) = self.child.try_lock()
            && let Some(child) = slot.as_mut()
        {
            let _ = child.kill();
        }
    }

    /// Stop the server and wait for it, bounded by [`STOP_TIMEOUT`].
    pub(crate) fn shutdown(&self) {
        self.shutting_down.store(true, Ordering::SeqCst);
        let _ = self.control.try_send(Control::Shutdown);
        if let Some(mut child) = lock(&self.child).take() {
            stop_child(&mut child);
        }
    }

    pub(crate) fn is_shutting_down(&self) -> bool {
        self.shutting_down.load(Ordering::SeqCst)
    }
}

/// How one server attempt ended.
enum Outcome {
    Shutdown,
    Restart,
    Stopped { ran_for: Duration, message: String },
}

/// Run the dashboard server until the app exits, restarting it when it stops.
pub(crate) fn supervise(app: AppHandle, sidecar: Arc<Sidecar>, control: Receiver<Control>) {
    let mut failures = 0u32;
    loop {
        if sidecar.is_shutting_down() {
            return;
        }
        set_status(
            &app,
            &sidecar,
            status_value("starting", "Starting the Orbit dashboard…", None),
        );
        let wait = match resolve_orbit() {
            Err(message) => {
                set_status(
                    &app,
                    &sidecar,
                    status_value("error", "Orbit was not found", Some(message)),
                );
                None
            }
            Ok(binary) => match run_server(&app, &sidecar, &control, &binary) {
                Outcome::Shutdown => return,
                Outcome::Restart => {
                    failures = 0;
                    show_home(&app, &sidecar);
                    continue;
                }
                Outcome::Stopped { ran_for, message } => {
                    failures = if ran_for >= HEALTHY_RUN {
                        0
                    } else {
                        failures + 1
                    };
                    let delay = backoff(failures);
                    show_home(&app, &sidecar);
                    set_status(
                        &app,
                        &sidecar,
                        status_value(
                            "error",
                            "The dashboard server stopped",
                            Some(format!(
                                "{message} Restarting in {} s.",
                                delay.as_secs().max(1)
                            )),
                        ),
                    );
                    Some(delay)
                }
            },
        };
        let next = match wait {
            Some(delay) => match control.recv_timeout(delay) {
                Ok(next) => Some(next),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => Some(Control::Shutdown),
            },
            None => Some(control.recv().unwrap_or(Control::Shutdown)),
        };
        if matches!(next, Some(Control::Shutdown)) {
            return;
        }
    }
}

fn backoff(failures: u32) -> Duration {
    match failures {
        0 => Duration::from_secs(1),
        n => Duration::from_secs(1u64 << n.min(5)).min(MAX_BACKOFF),
    }
}

fn run_server(
    app: &AppHandle,
    sidecar: &Arc<Sidecar>,
    control: &Receiver<Control>,
    binary: &Path,
) -> Outcome {
    let started = Instant::now();
    let stopped = |message: String| Outcome::Stopped {
        ran_for: started.elapsed(),
        message,
    };
    let log_path = log_path(app);
    let log = match open_log(log_path.as_deref()) {
        Ok(log) => log,
        Err(error) => return stopped(format!("Could not open the server log: {error}.")),
    };
    let log_hint = log_path
        .as_deref()
        .map(|path| format!(" See {}.", path.display()))
        .unwrap_or_default();

    let mut command = Command::new(binary);
    command
        .args(["web", "serve", "--port", "0", "--no-open", "--operator"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(
            log.try_clone()
                .map(Stdio::from)
                .unwrap_or_else(|_| Stdio::null()),
        );
    if let Some(home) = std::env::home_dir() {
        command.current_dir(home);
    }
    die_with_parent(&mut command);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return stopped(format!("Could not start {}: {error}.", binary.display())),
    };

    let (ready_tx, ready_rx) = mpsc::sync_channel::<Url>(1);
    if let Some(stdout) = child.stdout.take() {
        let mut log = log;
        std::thread::spawn(move || {
            let mut ready = Some(ready_tx);
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let _ = writeln!(log, "{line}");
                if let Some(address) = line.strip_prefix(LISTENING_PREFIX)
                    && let Ok(url) = Url::parse(address.trim())
                    && let Some(tx) = ready.take()
                {
                    let _ = tx.send(url);
                }
            }
        });
    }
    *lock(&sidecar.child) = Some(child);

    // Wait for the address, then for the server to stop.
    let mut ready = false;
    loop {
        if !ready {
            match ready_rx.try_recv() {
                Ok(url) => {
                    ready = true;
                    show_dashboard(app, sidecar, url);
                }
                Err(_) if started.elapsed() > READY_TIMEOUT => {
                    if let Some(mut child) = lock(&sidecar.child).take() {
                        stop_child(&mut child);
                    }
                    return stopped(format!(
                        "The server did not report its address within {} s.{log_hint}",
                        READY_TIMEOUT.as_secs()
                    ));
                }
                Err(_) => {}
            }
        }
        match control.recv_timeout(POLL) {
            Ok(Control::Shutdown) | Err(RecvTimeoutError::Disconnected) => {
                return Outcome::Shutdown;
            }
            Ok(Control::Restart) => {
                *lock(&sidecar.dashboard) = None;
                if let Some(mut child) = lock(&sidecar.child).take() {
                    stop_child(&mut child);
                }
                return if sidecar.is_shutting_down() {
                    Outcome::Shutdown
                } else {
                    Outcome::Restart
                };
            }
            Ok(Control::Retry) | Err(RecvTimeoutError::Timeout) => {}
        }
        if let Some(status) = reap(sidecar) {
            if sidecar.is_shutting_down() {
                return Outcome::Shutdown;
            }
            *lock(&sidecar.dashboard) = None;
            let when = if ready { "" } else { " before it was ready" };
            return stopped(format!("It exited{when} ({}).{log_hint}", describe(status)));
        }
    }
}

/// The child's exit status once it has exited, clearing the slot.
fn reap(sidecar: &Sidecar) -> Option<ExitStatus> {
    let mut slot = lock(&sidecar.child);
    let status = slot
        .as_mut()
        .and_then(|child| child.try_wait().ok().flatten())?;
    *slot = None;
    Some(status)
}

fn describe(status: ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("exit code {code}"),
        None => status.to_string(),
    }
}

fn show_dashboard(app: &AppHandle, sidecar: &Sidecar, url: Url) {
    *lock(&sidecar.dashboard) = Some(url.clone());
    set_status(
        app,
        sidecar,
        status_value("ready", "Opening the dashboard…", None),
    );
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        let _ = window.navigate(url);
    }
}

fn show_home(app: &AppHandle, sidecar: &Sidecar) {
    if sidecar.on_status_page.load(Ordering::SeqCst) {
        return;
    }
    if let Some(window) = app.get_webview_window(MAIN_WINDOW)
        && let Ok(url) = Url::parse(STATUS_PAGE)
    {
        let _ = window.navigate(url);
    }
}

/// Record the status and push it to the status page if that is what the
/// window shows; the page also reads it on load.
fn set_status(app: &AppHandle, sidecar: &Sidecar, status: Value) {
    let script = format!("window.orbitDesktop && window.orbitDesktop.render({status});");
    *lock(&sidecar.status) = status;
    if sidecar.on_status_page.load(Ordering::SeqCst)
        && let Some(window) = app.get_webview_window(MAIN_WINDOW)
    {
        let _ = window.eval(script);
    }
}

/// The bundled status page, at the address Tauri serves app assets from.
#[cfg(not(windows))]
const STATUS_PAGE: &str = "tauri://localhost/index.html";
#[cfg(windows)]
const STATUS_PAGE: &str = "http://tauri.localhost/index.html";

fn is_status_page(url: &Url) -> bool {
    match url.scheme() {
        "tauri" => url.host_str() == Some("localhost"),
        "http" | "https" => url.host_str() == Some("tauri.localhost"),
        _ => false,
    }
}

fn status_value(phase: &str, title: &str, detail: Option<String>) -> Value {
    json!({ "phase": phase, "title": title, "detail": detail })
}

/// Ask the server to drain, then kill it if it outlives [`STOP_TIMEOUT`].
fn stop_child(child: &mut Child) {
    #[cfg(unix)]
    if let Ok(pid) = libc::pid_t::try_from(child.id()) {
        // SAFETY: plain syscall on our own child's pid, which `child` still
        // owns (not yet reaped), so the pid cannot have been reused.
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
        let deadline = Instant::now() + STOP_TIMEOUT;
        while Instant::now() < deadline {
            if child.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// On Linux, have the kernel stop the server if the app dies without running
/// its exit handler. macOS has no equivalent; there the server outlives a
/// crashed app until it is stopped by hand.
fn die_with_parent(command: &mut Command) {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: `prctl` is async-signal-safe and touches no shared state.
        // PDEATHSIG fires when the spawning thread exits; the supervisor
        // thread spawns the server and lives until the app exits.
        unsafe {
            command.pre_exec(|| {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = command;
}

fn log_path(app: &AppHandle) -> Option<PathBuf> {
    app.path()
        .app_log_dir()
        .ok()
        .map(|dir| dir.join("dashboard-server.log"))
}

fn open_log(path: Option<&Path>) -> std::io::Result<std::fs::File> {
    let Some(path) = path else {
        return std::fs::OpenOptions::new().write(true).open(null_device());
    };
    // Server output can name hosts, paths and tasks: keep it owner-only.
    if let Some(dir) = path.parent() {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder.create(dir)?;
    }
    let oversized = std::fs::metadata(path).is_ok_and(|meta| meta.len() > MAX_LOG_BYTES);
    let mut options = std::fs::OpenOptions::new();
    options.create(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    if oversized {
        options.write(true).truncate(true);
    } else {
        options.append(true);
    }
    options.open(path)
}

fn null_device() -> &'static str {
    if cfg!(windows) { "NUL" } else { "/dev/null" }
}

/// Find the user's `orbit` binary.
///
/// An app started from the Dock or a desktop launcher gets a minimal `PATH`,
/// so the installers' default locations are checked as well as `PATH`.
/// `ORBIT_BIN` overrides the search.
fn resolve_orbit() -> Result<PathBuf, String> {
    if let Some(explicit) = std::env::var_os("ORBIT_BIN") {
        let path = PathBuf::from(explicit);
        return runnable(&path).ok_or_else(|| {
            format!(
                "ORBIT_BIN is set to {}, which is not an executable file.",
                path.display()
            )
        });
    }
    candidates()
        .iter()
        .find_map(|path| runnable(path))
        .ok_or_else(|| {
            "Install it with `npm install -g @orbit-tools/cli`, or set ORBIT_BIN to the orbit \
             executable, then retry."
                .to_string()
        })
}

fn candidates() -> Vec<PathBuf> {
    let exe = if cfg!(windows) { "orbit.exe" } else { "orbit" };
    let home = std::env::home_dir();
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(dir) = std::env::var_os("ORBIT_INSTALL_DIR") {
        dirs.push(dir.into());
    }
    if let Some(home) = &home {
        dirs.push(home.join(".orbit/bin"));
    }
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    if let Some(home) = &home {
        dirs.extend([".cargo/bin", ".local/bin", ".npm-global/bin"].map(|dir| home.join(dir)));
        // nvm keeps one global bin per installed Node version.
        if let Ok(versions) = std::fs::read_dir(home.join(".nvm/versions/node")) {
            dirs.extend(
                versions
                    .filter_map(Result::ok)
                    .map(|entry| entry.path().join("bin")),
            );
        }
    }
    dirs.extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"].map(PathBuf::from));
    dirs.into_iter().map(|dir| dir.join(exe)).collect()
}

/// The native binary behind `path`, if it can be run.
///
/// The npm package's `orbit` is a Node launcher, which fails without `node`
/// on `PATH`; its native binary sits in the package's `binaries/` directory.
fn runnable(path: &Path) -> Option<PathBuf> {
    let target = std::fs::canonicalize(path).ok()?;
    if target.extension().is_some_and(|ext| ext == "js") {
        let package = target.parent()?.parent()?;
        let name = if cfg!(windows) { "orbit.exe" } else { "orbit" };
        return executable(&package.join("binaries").join(name));
    }
    executable(&target)
}

fn executable(path: &Path) -> Option<PathBuf> {
    let meta = std::fs::metadata(path).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o111 == 0 {
            return None;
        }
    }
    meta.is_file().then(|| path.to_path_buf())
}

/// Lock a mutex, recovering the data if a panicking thread poisoned it.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
