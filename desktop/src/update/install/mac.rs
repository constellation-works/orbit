//! Verify and replace the installed macOS app bundle.

use std::ffi::OsStr;
use std::fmt::{Display, Formatter};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::{
    BundleSource, Outcome, UpdateBundle, attempt_token, download_exclusive, file_name_of, writable,
};

/// The bundle's name inside the release disk image.
const DMG_APP: &str = "Orbit.app";
/// Output kept from each stream of a helper tool.
const MAX_CAPTURE: u64 = 64 * 1024;
/// Poll interval while a helper tool runs.
const POLL: Duration = Duration::from_millis(50);
/// Deadlines for the helper tools: reading a signature or a plist, and
/// mounting, verifying or copying a whole bundle.
const QUICK: Duration = Duration::from_secs(30);
const SLOW: Duration = Duration::from_secs(10 * 60);

/// `MAJOR.MINOR.PATCH`, matching the release client's stable versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Version(u64, u64, u64);

impl Version {
    fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let text = text.strip_prefix('v').unwrap_or(text);
        let mut parts = text.split('.');
        let mut next = || -> Option<u64> {
            let part = parts.next()?;
            if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            part.parse().ok()
        };
        let version = Self(next()?, next()?, next()?);
        parts.next().is_none().then_some(version)
    }
}

impl Display for Version {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}.{}.{}", self.0, self.1, self.2)
    }
}

/// What a helper tool printed, and whether it succeeded.
struct Captured {
    program: String,
    success: bool,
    stdout: String,
    stderr: String,
}

impl Captured {
    fn succeeded(self) -> Result<Self, String> {
        if self.success {
            Ok(self)
        } else {
            Err(format!("{} failed: {}", self.program, self.stderr.trim()))
        }
    }
}

/// A private temporary folder, removed with everything in it.
struct Staging(PathBuf);

impl Staging {
    fn new() -> Result<Self, String> {
        // A fresh directory per attempt. Deleting `orbit-desktop-update-<pid>`
        // would remove another attempt's disk image in this same process.
        let path = std::env::temp_dir().join(format!("orbit-desktop-update-{}", attempt_token()));
        private_dir(&path)?;
        Ok(Self(path))
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A mounted disk image, detached when dropped.
struct Attached(PathBuf);

impl Drop for Attached {
    fn drop(&mut self) {
        let _ = run(
            Command::new("/usr/bin/hdiutil")
                .args(["detach", "-force", "-quiet"])
                .arg(&self.0),
            QUICK,
        );
    }
}

/// Why `bundle` can't be replaced in place, if it can't.
pub(super) fn blocked(bundle: &Path) -> Option<String> {
    let text = bundle.to_string_lossy();
    if text.contains("/AppTranslocation/") {
        return Some(
            "macOS is running it from a temporary copy; move Orbit to Applications".into(),
        );
    }
    if text.starts_with("/Volumes/") {
        return Some("it is running from the disk image".into());
    }
    match bundle.parent() {
        Some(parent) if writable(parent) => {}
        Some(parent) => return Some(format!("{} isn't writable", parent.display())),
        None => return Some("it has no parent folder".into()),
    }
    if team_id(bundle).is_none() {
        return Some("this copy isn't signed with a Developer ID".into());
    }
    None
}

/// The Team ID the running bundle is signed with.
fn team_id(bundle: &Path) -> Option<String> {
    let output = run(
        Command::new("/usr/bin/codesign")
            .args(["-dv", "--verbose=2"])
            .arg(bundle),
        QUICK,
    )
    .ok()?;
    // codesign prints the signature details on stderr.
    output
        .stderr
        .lines()
        .find_map(|line| line.strip_prefix("TeamIdentifier="))
        .map(str::trim)
        .filter(|team| !team.is_empty() && team.bytes().all(|byte| byte.is_ascii_alphanumeric()))
        .map(str::to_string)
}

pub(super) fn replace(
    source: &impl BundleSource,
    update: &UpdateBundle,
    bundle: &Path,
    identifier: &str,
) -> Result<Outcome, String> {
    let team = team_id(bundle).ok_or("the running app's Team ID could not be read")?;
    let bundle_name = file_name_of(&update.bundle)?;
    let staging = Staging::new()?;
    let dmg = download_exclusive(source, update, &staging.0, OsStr::new(bundle_name), 0o600)?;
    let mount = staging.0.join("mount");
    private_dir(&mount)?;
    run(
        Command::new("/usr/bin/hdiutil")
            .args([
                "attach",
                "-nobrowse",
                "-readonly",
                "-noautoopen",
                "-mountpoint",
            ])
            .arg(&mount)
            .arg(dmg.path()),
        SLOW,
    )?
    .succeeded()?;
    let attached = Attached(mount.clone());
    let new_app = mount.join(DMG_APP);
    let requirement = format!(
        "=anchor apple generic and identifier \"{identifier}\" and certificate leaf[subject.OU] = \"{team}\""
    );
    run(
        Command::new("/usr/bin/codesign")
            .args(["--verify", "--deep", "--strict", "-R"])
            .arg(&requirement)
            .arg(&new_app),
        SLOW,
    )
    .and_then(Captured::succeeded)
    .map_err(|error| format!("the update's Apple signature was rejected: {error}"))?;
    let reported = bundle_version(&new_app);
    let expected = Version::parse(&update.version);
    if reported.is_none() || expected.is_none() || reported != expected {
        return Err(format!(
            "the update's app reports version {}, not {}",
            reported.map_or_else(|| "nothing".into(), |version| version.to_string()),
            update.version
        ));
    }

    let parent = bundle
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .ok_or("the app has no parent folder")?;
    // Names include this attempt's token. Nothing here deletes another
    // attempt's staged bundle or its backup, including one left by a crash.
    let token = attempt_token();
    let staged = parent.join(format!(".Orbit.app.update.{token}"));
    let previous = parent.join(format!(".Orbit.app.previous.{token}"));
    run(
        Command::new("/usr/bin/ditto").arg(&new_app).arg(&staged),
        SLOW,
    )
    .and_then(Captured::succeeded)
    .inspect_err(|_| {
        let _ = std::fs::remove_dir_all(&staged);
    })?;
    drop(attached);
    // The swap is several renames. One lock covers the move aside, the
    // publish, and putting the previous bundle back if the publish fails.
    let _lock = exclusive_lock(&parent.join(".orbit-desktop-update.lock")).inspect_err(|_| {
        let _ = std::fs::remove_dir_all(&staged);
    })?;
    std::fs::rename(bundle, &previous).map_err(|error| {
        let _ = std::fs::remove_dir_all(&staged);
        format!("could not move {} aside: {error}", bundle.display())
    })?;
    if let Err(error) = std::fs::rename(&staged, bundle) {
        let restored = std::fs::rename(&previous, bundle);
        let _ = std::fs::remove_dir_all(&staged);
        if let Err(restore_error) = restored {
            return Err(format!(
                "could not install the update: {error}; restoring {} also failed: {restore_error}",
                bundle.display()
            ));
        }
        return Err(format!("could not install the update: {error}"));
    }
    let _ = std::fs::remove_dir_all(&previous);
    Ok(Outcome::Restart(vec![
        "/usr/bin/open".into(),
        bundle.as_os_str().to_owned(),
    ]))
}

/// Block until this process holds the advisory lock at `path`.
///
/// The lock file stays in place so the next attempt can wait on the same
/// inode. Closing the returned file releases the lock.
fn exclusive_lock(path: &Path) -> Result<std::fs::File, String> {
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::io::AsRawFd;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)
        .map_err(|error| format!("could not lock {}: {error}", path.display()))?;
    // SAFETY: `file` stays open for the call. LOCK_EX waits until this
    // process holds the advisory lock, and the kernel releases that lock
    // when `file` is closed, including if this process exits.
    let locked = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
    if locked != 0 {
        return Err(format!(
            "could not lock {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        ));
    }
    Ok(file)
}

fn bundle_version(app: &Path) -> Option<Version> {
    let output = run(
        Command::new("/usr/libexec/PlistBuddy")
            .args(["-c", "Print :CFBundleShortVersionString"])
            .arg(app.join("Contents/Info.plist")),
        QUICK,
    )
    .and_then(Captured::succeeded)
    .ok()?;
    Version::parse(&output.stdout)
}

fn private_dir(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(path)
        .map_err(|error| format!("could not prepare the update: {error}"))
}

/// Run a helper tool to completion within `timeout`, in its own process
/// group, keeping at most [`MAX_CAPTURE`] bytes of each output stream.
fn run(command: &mut Command, timeout: Duration) -> Result<Captured, String> {
    use std::os::unix::process::CommandExt;
    let program = Path::new(command.get_program()).display().to_string();
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|error| format!("could not run {program}: {error}"))?;
    let stdout = capture(child.stdout.take());
    let stderr = capture(child.stderr.take());
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(POLL),
            Ok(None) => {
                if let Ok(group) = i32::try_from(child.id()) {
                    // SAFETY: signals only the child's own process group.
                    unsafe { libc::kill(-group, libc::SIGKILL) };
                }
                let _ = child.wait();
                return Err(format!(
                    "{program} didn't finish within {} seconds",
                    timeout.as_secs()
                ));
            }
            Err(error) => return Err(format!("could not wait for {program}: {error}")),
        }
    };
    Ok(Captured {
        program,
        success: status.success(),
        stdout: stdout.join().unwrap_or_default(),
        stderr: stderr.join().unwrap_or_default(),
    })
}

/// Read a pipe on its own thread, keeping the first [`MAX_CAPTURE`] bytes
/// and discarding the rest so the child never blocks on a full pipe.
fn capture(pipe: Option<impl Read + Send + 'static>) -> JoinHandle<String> {
    std::thread::spawn(move || {
        let mut kept = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = (&mut pipe).take(MAX_CAPTURE).read_to_end(&mut kept);
            let _ = std::io::copy(&mut pipe, &mut std::io::sink());
        }
        String::from_utf8_lossy(&kept).into_owned()
    })
}
