//! Private local discovery and browser launch for the session API.

use std::io::{IsTerminal, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use clap::Args;
use orbit_common::fs::{io as private_io, path};
use orbit_config::{ConfigRoots, ResolvedConfig};
use orbit_core::OrbitError;
use serde::{Deserialize, Serialize};

use crate::api::session::{self, BrowserSessions};

/// Arguments for reopening a running local dashboard with a fresh ticket.
#[derive(Args, Clone)]
#[command(about = "Open a running dashboard with a fresh browser-session URL")]
pub struct OpenArgs {
    /// Print the private URL without opening a browser. Treat it as a credential.
    #[arg(long)]
    pub no_open: bool,
}

// Deliberately no Debug: diagnostics must never include this file's contents.
#[derive(Serialize, Deserialize)]
struct SessionFile {
    key: [u8; 32],
    address: SocketAddr,
}

fn session_directory() -> Result<PathBuf, OrbitError> {
    // Like SSH control sockets, host credentials must not follow --root into
    // arbitrary workspaces. Every confinement masks this machine-global tree.
    Ok(path::global_orbit_dir()?.join("run"))
}

fn private_directory() -> Result<PathBuf, OrbitError> {
    let directory = session_directory()?;
    private_io::create_private_dir_all(&directory).map_err(session_io_error)?;
    check_private_path(&directory, true)?;
    Ok(directory)
}

fn check_private_path(path: &Path, directory: bool) -> Result<(), OrbitError> {
    let metadata = std::fs::symlink_metadata(path).map_err(session_io_error)?;
    if metadata.file_type().is_symlink()
        || metadata.is_dir() != directory
        || (!directory && !metadata.is_file())
    {
        return Err(OrbitError::PolicyDenied(
            "browser credential path must not be a symlink or special file".into(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: geteuid has no arguments or memory-safety preconditions.
        let uid = unsafe { libc::geteuid() };
        let mode = if directory { 0o700 } else { 0o600 };
        if metadata.uid() != uid
            || metadata.mode() & 0o777 != mode
            || (!directory && metadata.nlink() != 1)
        {
            return Err(OrbitError::PolicyDenied(format!(
                "browser credential {} must be owned by this user with mode {mode:o}",
                if directory { "directory" } else { "file" }
            )));
        }
    }
    #[cfg(not(unix))]
    return Err(OrbitError::PolicyDenied(
        "browser sessions require Unix private-file permissions".into(),
    ));
    #[cfg(unix)]
    Ok(())
}

fn session_io_error(error: std::io::Error) -> OrbitError {
    OrbitError::Io(format!("browser credential file: {error}"))
}

/// Initialize only after the listener binds, so a failed second startup does
/// not replace a running dashboard's key. Settings are host-global and frozen.
pub(crate) fn start_browser_sessions(
    config_root: &Path,
    address: SocketAddr,
) -> Result<Option<(Arc<BrowserSessions>, String)>, OrbitError> {
    let config = ResolvedConfig::load(&ConfigRoots::global_only(config_root))?;
    if !config.snapshot.machine_web_terminal_enabled {
        return Ok(None);
    }
    let directory = private_directory()?;
    let key = session::random_key()?;
    let sessions = Arc::new(BrowserSessions::from_key(
        key,
        config.snapshot.machine_web_session_ttl,
    ));
    let file = directory.join("web-session.secret");
    if file.try_exists().map_err(session_io_error)? {
        check_private_path(&file, false)?;
    }
    let contents = serde_json::to_string(&SessionFile { key, address })
        .map_err(|_| OrbitError::Execution("encode browser credential file".into()))?;
    private_io::atomic_write_text(&file, &contents).map_err(session_io_error)?;
    let url = format!(
        "http://{address}/#orbit-session={}",
        session::browser_ticket(&key)?
    );
    Ok(Some((sessions, url)))
}

/// Print and optionally open a fresh one-time URL for the local dashboard.
///
/// Credentials always live in `~/.orbit/run`, independently of the registry
/// selected by `--root`. The live instance must match the private file.
pub fn open_dashboard(args: OpenArgs) -> Result<(), OrbitError> {
    let directory = session_directory()?;
    let file = directory.join("web-session.secret");
    if !file.try_exists().map_err(session_io_error)? {
        return Err(OrbitError::InvalidInput("no terminal-enabled dashboard credential exists; start the dashboard with machine.web_terminal.enabled = true".into()));
    }
    check_private_path(&directory, true)?;
    check_private_path(&file, false)?;
    let mut contents = String::new();
    private_io::open_read_only_no_follow(&file)
        .map_err(session_io_error)?
        .take(4096)
        .read_to_string(&mut contents)
        .map_err(session_io_error)?;
    let record: SessionFile = serde_json::from_str(&contents).map_err(|_| {
        OrbitError::InvalidInput("invalid browser credential file; restart the dashboard".into())
    })?;
    if !record.address.ip().is_loopback() || record.address.port() == 0 {
        return Err(OrbitError::InvalidInput(
            "invalid dashboard address in browser credential file".into(),
        ));
    }
    let identity = probe_session_identity(record.address)?;
    let expected = BrowserSessions::from_key(record.key, 1).identity();
    // These are public boot identifiers, but keep even this comparison constant-time.
    if blake3::hash(identity.as_bytes()) != blake3::hash(expected.as_bytes()) {
        return Err(OrbitError::InvalidInput(
            "dashboard credential is stale; restart the terminal-enabled dashboard".into(),
        ));
    }
    let url = format!(
        "http://{}/#orbit-session={}",
        record.address,
        session::browser_ticket(&record.key)?
    );
    #[allow(
        clippy::print_stdout,
        reason = "the operator copies the one-use dashboard URL from stdout"
    )]
    {
        println!("{url}");
    }
    if !args.no_open {
        open_private_browser_url(&url)?;
    }
    Ok(())
}

fn probe_session_identity(address: SocketAddr) -> Result<String, OrbitError> {
    let unavailable =
        || OrbitError::Execution("no running dashboard at the credential's local address".into());
    let timeout = Duration::from_secs(2);
    let mut stream = TcpStream::connect_timeout(&address, timeout).map_err(|_| unavailable())?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|_| unavailable())?;
    write!(
        stream,
        "GET /api/session HTTP/1.0\r\nHost: {address}\r\nConnection: close\r\n\r\n"
    )
    .map_err(|_| unavailable())?;
    let deadline = Instant::now() + timeout;
    let mut bytes = Vec::new();
    let mut chunk = [0; 1024];
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() || bytes.len() > 8192 {
            return Err(unavailable());
        }
        stream
            .set_read_timeout(Some(remaining))
            .map_err(|_| unavailable())?;
        let count = stream.read(&mut chunk).map_err(|_| unavailable())?;
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
    let response = std::str::from_utf8(&bytes).map_err(|_| unavailable())?;
    let status = response
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1));
    if status == Some("404") {
        return Err(OrbitError::InvalidInput(
            "running dashboard has browser sessions disabled".into(),
        ));
    }
    if status != Some("200") {
        return Err(unavailable());
    }
    let (_, body) = response.split_once("\r\n\r\n").ok_or_else(unavailable)?;
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("instance")
                .and_then(|value| value.as_str())
                .map(str::to_owned)
        })
        .ok_or_else(unavailable)
}

/// Startup logs show only the base URL. An interactive terminal may display a
/// private ticket; redirected stdout (services, SSH, audit capture) never does.
pub(crate) fn print_interactive_session_url(url: &str) {
    if std::io::stdout().is_terminal() {
        #[allow(
            clippy::print_stdout,
            reason = "a terminal operator must see the one-use session URL and redirected stdout must stay quiet"
        )]
        {
            println!("Browser session (one use): {url}");
        }
    }
}

/// Launch a private redirect document, so neither the platform opener nor the
/// browser's argv contains a ticket. The entire run directory is sandbox-masked.
pub(crate) fn open_private_browser_url(url: &str) -> Result<(), OrbitError> {
    let parsed = url::Url::parse(url)
        .map_err(|_| OrbitError::InvalidInput("invalid browser session URL".into()))?;
    let ticket = parsed
        .fragment()
        .and_then(|fragment| fragment.strip_prefix("orbit-session="))
        .ok_or_else(|| OrbitError::InvalidInput("missing browser session ticket".into()))?;
    if parsed.scheme() != "http"
        || !matches!(parsed.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
        || parsed.path() != "/"
        || parsed.query().is_some()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || URL_SAFE_NO_PAD
            .decode(ticket)
            .ok()
            .is_none_or(|bytes| bytes.len() != 72)
    {
        return Err(OrbitError::InvalidInput(
            "invalid browser session URL".into(),
        ));
    }
    let directory = private_directory()?;
    let name = URL_SAFE_NO_PAD.encode(session::random_key()?);
    let file = directory.join(format!("web-launch-{name}.html"));
    // URL components above admit no HTML delimiters. No credential-bearing
    // document is served by the dashboard or placed in its asset tree.
    private_io::write_new_private_text(&file, &format!("<!doctype html><meta name=referrer content=no-referrer><meta http-equiv=refresh content=\"0;url={url}\"><title>Opening Orbit</title>Opening Orbit…"))
        .map_err(session_io_error)?;
    crate::open_browser(&file.to_string_lossy());
    Ok(())
}
