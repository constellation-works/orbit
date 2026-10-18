//! Installing a downloaded update, which depends on how the app was installed.
//!
//! - A macOS `.app` the user can write to is replaced in place. The new bundle
//!   comes out of the release's `.dmg`. It must pass `codesign --verify
//!   --deep --strict` against the running bundle's identifier and Team ID, and
//!   report the version the release signed. A bundle the app can't replace
//!   (read-only, translocated, run from the disk image, or unsigned) gets the
//!   verified `.dmg` opened instead.
//! - A Linux AppImage (`$APPIMAGE`) is replaced by the new one. If its
//!   directory isn't writable, the new AppImage is saved to Downloads.
//! - Any other Linux install, a `.deb` in practice, needs root to upgrade. The
//!   app never asks for root: it saves the verified `.deb` to Downloads and
//!   says how to install it.
//!
//! Every bundle is checked against the signed manifest before anything is
//! replaced or handed over, and partial downloads are discarded.
//!
//! Two installs may run at once (each app process has its own updater, and
//! nothing stops a second process). Each attempt therefore stages into a new
//! file that only it creates. Publishing links that verified inode into the
//! destination directory and renames the link, so a commit cannot follow a
//! staging name another attempt has replaced. Cleanup unlinks a staging name
//! only while it still refers to that attempt's file. The macOS bundle swap
//! uses the same per-attempt names and holds one lock across the replace and
//! its recovery.

use std::ffi::{CString, OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// A verified release asset the installer can place on disk.
///
/// `version` is the stable `MAJOR.MINOR.PATCH` the macOS bundle must report.
/// `bundle` is a single file name, the asset's name in the release.
#[derive(Debug, Clone)]
pub struct UpdateBundle {
    /// Release version, `MAJOR.MINOR.PATCH`.
    pub version: String,
    /// Asset file name, with no directory components.
    pub bundle: String,
}

/// Writes one verified bundle.
///
/// The desktop's release client checks the signed digest before it returns
/// `Ok`. A failure may leave a short file; the installer discards that file
/// and does not publish it. Tests supply a transport that pauses at chosen
/// points instead of reading the network.
pub trait BundleSource {
    /// Stream `update` into `file`, failing unless those bytes are the
    /// verified bundle.
    fn download(&self, update: &UpdateBundle, file: &mut File) -> Result<(), String>;
}

/// How this copy of the app was installed.
pub enum Install {
    /// A macOS app bundle.
    #[cfg(target_os = "macos")]
    MacApp {
        /// The running `.app` bundle.
        bundle: PathBuf,
        /// Why the bundle can't be replaced in place, if it can't.
        blocked: Option<String>,
    },
    /// A Linux AppImage at this path.
    #[cfg(target_os = "linux")]
    AppImage(PathBuf),
    /// Any other Linux install, normally the `.deb`.
    #[cfg(target_os = "linux")]
    Package,
}

/// What the caller does once an install step succeeds.
#[derive(Debug)]
pub enum Outcome {
    /// The app was replaced: run this command once the app has exited.
    Restart(Vec<OsString>),
    /// The verified bundle was saved for the user to install.
    Saved {
        /// Where the verified bundle was written.
        path: PathBuf,
        /// Open the file (a disk image) rather than reveal it.
        open: bool,
        /// What to tell the user.
        message: String,
    },
}

impl Install {
    /// How the running app was installed. `None` for a native development build or
    /// a platform without a desktop release.
    pub fn detect() -> Option<Self> {
        #[cfg(feature = "native")]
        if tauri::is_dev() {
            return None;
        }
        detect_platform()
    }

    /// The release asset this install updates from.
    pub fn asset(&self) -> Option<&'static str> {
        match self {
            #[cfg(target_os = "macos")]
            Self::MacApp { .. } => Some("orbit-desktop-universal-apple-darwin.dmg"),
            #[cfg(target_os = "linux")]
            Self::AppImage(_) => match std::env::consts::ARCH {
                "x86_64" => Some("orbit-desktop-x86_64-unknown-linux-gnu.AppImage"),
                "aarch64" => Some("orbit-desktop-aarch64-unknown-linux-gnu.AppImage"),
                _ => None,
            },
            #[cfg(target_os = "linux")]
            Self::Package => match std::env::consts::ARCH {
                "x86_64" => Some("orbit-desktop-x86_64-unknown-linux-gnu.deb"),
                "aarch64" => Some("orbit-desktop-aarch64-unknown-linux-gnu.deb"),
                _ => None,
            },
        }
    }

    /// The confirmation dialog's text: what installing will do.
    pub fn describe(&self) -> String {
        let checked =
            "Orbit downloads the update and checks it against the release's signed checksums";
        match self {
            #[cfg(target_os = "macos")]
            Self::MacApp {
                bundle,
                blocked: None,
            } => format!(
                "{checked} and its Apple signature, replaces {}, then restarts.",
                bundle.display()
            ),
            #[cfg(target_os = "macos")]
            Self::MacApp {
                blocked: Some(reason),
                ..
            } => format!(
                "{checked}, then opens the disk image for you to copy Orbit into Applications. \
                 Orbit can't replace itself because {reason}."
            ),
            #[cfg(target_os = "linux")]
            Self::AppImage(path) => {
                format!("{checked}, replaces {}, then restarts.", path.display())
            }
            #[cfg(target_os = "linux")]
            Self::Package => format!(
                "{checked}, then saves the .deb to your Downloads folder for you to install."
            ),
        }
    }

    /// Download `update`, verify it, and install or hand it over.
    pub fn install(
        &self,
        source: &impl BundleSource,
        update: &UpdateBundle,
        identifier: &str,
        downloads: &Path,
    ) -> Result<Outcome, String> {
        match self {
            #[cfg(target_os = "macos")]
            Self::MacApp {
                bundle,
                blocked: None,
            } => mac::replace(source, update, bundle, identifier),
            #[cfg(target_os = "macos")]
            Self::MacApp { .. } => {
                let path = save(source, update, downloads)?;
                Ok(Outcome::Saved {
                    path,
                    open: true,
                    message: "Quit Orbit, then drag Orbit from the disk image into Applications \
                              to replace it."
                        .into(),
                })
            }
            #[cfg(target_os = "linux")]
            Self::AppImage(path) => {
                let _ = identifier;
                match replace_file(source, update, path) {
                    Ok(()) => Ok(Outcome::Restart(vec![path.clone().into_os_string()])),
                    Err(Replace::Blocked(reason)) => {
                        let saved = save(source, update, downloads)?;
                        Ok(Outcome::Saved {
                            message: format!(
                                "{reason}. Replace {} with {} to finish.",
                                path.display(),
                                saved.display()
                            ),
                            path: saved,
                            open: false,
                        })
                    }
                    Err(Replace::Failed(error)) => Err(error),
                }
            }
            #[cfg(target_os = "linux")]
            Self::Package => {
                let _ = identifier;
                let path = save(source, update, downloads)?;
                Ok(Outcome::Saved {
                    message: format!("Install it with: sudo apt install {}", path.display()),
                    path,
                    open: false,
                })
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn detect_platform() -> Option<Install> {
    let exe = std::env::current_exe().ok()?;
    // …/Orbit.app/Contents/MacOS/<binary>
    let bundle = exe.parent()?.parent()?.parent()?.to_path_buf();
    if bundle
        .extension()
        .is_none_or(|extension| extension != "app")
    {
        return None;
    }
    let blocked = mac::blocked(&bundle);
    Some(Install::MacApp { bundle, blocked })
}

#[cfg(target_os = "linux")]
fn detect_platform() -> Option<Install> {
    match std::env::var_os("APPIMAGE").map(PathBuf::from) {
        Some(path) if path.is_absolute() && path.is_file() => Some(Install::AppImage(path)),
        _ => Some(Install::Package),
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn detect_platform() -> Option<Install> {
    None
}

/// Token that makes one attempt's staging name distinct from every other
/// attempt, including another thread in this process.
fn attempt_token() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let serial = NEXT.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("{}-{serial}-{nanos}", std::process::id())
}

/// A file this attempt created and still holds open.
///
/// Dropping it unlinks the staging name only when that name still refers to
/// this file. A name another attempt replaced is left alone.
struct StagedBytes {
    file: File,
    path: PathBuf,
}

impl StagedBytes {
    #[cfg(target_os = "macos")]
    fn path(&self) -> &Path {
        &self.path
    }

    /// Make `dest` name this verified inode.
    ///
    /// The link is taken from the open file, not from the staging path, so
    /// the directory entry that is renamed is the file that was just checked.
    fn commit(self, dest: &Path) -> io::Result<()> {
        publish_verified(&self.file, &self.path, dest)
    }
}

impl Drop for StagedBytes {
    fn drop(&mut self) {
        remove_file_if_same(&self.file, &self.path);
    }
}

/// Create a new staging file in `dir`. Never unlinks an existing name.
fn stage_new(dir: &Path, label: &OsStr, mode: u32) -> Result<StagedBytes, String> {
    if label.is_empty() || label == OsStr::new(".") || label == OsStr::new("..") {
        return Err(format!("refusing to stage {}", Path::new(label).display()));
    }
    let mut last_error = None;
    for _ in 0..8 {
        let mut name = OsString::from(".");
        name.push(label);
        name.push(".");
        name.push(attempt_token());
        if name.as_bytes().contains(&b'/')
            || name.as_bytes().contains(&b'\\')
            || name.as_bytes().contains(&0u8)
        {
            return Err(format!("refusing to stage {}", Path::new(label).display()));
        }
        let path = dir.join(&name);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&path)
        {
            Ok(file) => return Ok(StagedBytes { file, path }),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => last_error = Some(error),
            Err(error) => {
                return Err(format!("could not create {}: {error}", path.display()));
            }
        }
    }
    Err(format!(
        "could not create a staging file in {}: {}",
        dir.display(),
        last_error.map_or_else(
            || "the name was already taken".to_string(),
            |error| error.to_string()
        )
    ))
}

/// Download `update` into a new file under `dir`. The file is removed if the
/// download fails or the staged bytes are dropped without [`StagedBytes::commit`].
fn download_exclusive(
    source: &impl BundleSource,
    update: &UpdateBundle,
    dir: &Path,
    label: &OsStr,
    mode: u32,
) -> Result<StagedBytes, String> {
    let mut staged = stage_new(dir, label, mode)?;
    source.download(update, &mut staged.file)?;
    Ok(staged)
}

fn file_name_of(bundle: &str) -> Result<&str, String> {
    if bundle.is_empty()
        || bundle == "."
        || bundle == ".."
        || bundle.contains('/')
        || bundle.contains('\\')
        || bundle.contains('\0')
    {
        return Err(format!("refusing to use {bundle} as a file name"));
    }
    Ok(bundle)
}

/// Whether `path` is still a regular file naming the same inode as `file`.
fn same_inode(file: &File, path: &Path) -> bool {
    let Ok(open) = file.metadata() else {
        return false;
    };
    // `symlink_metadata` does not follow a link planted over the staging
    // name. Renaming that link would publish the link, not the verified file.
    let Ok(linked) = std::fs::symlink_metadata(path) else {
        return false;
    };
    linked.file_type().is_file() && open.dev() == linked.dev() && open.ino() == linked.ino()
}

fn remove_file_if_same(file: &File, path: &Path) {
    if same_inode(file, path) {
        let _ = std::fs::remove_file(path);
    }
}

fn c_path(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "path contains an interior NUL byte",
        )
    })
}

/// Add a second directory entry for the inode `file` has open.
fn link_open_file(file: &File, link_path: &Path) -> io::Result<()> {
    let fd_path = CString::new(format!("/proc/self/fd/{}", file.as_raw_fd())).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "file descriptor path contains a NUL byte",
        )
    })?;
    let dest = c_path(link_path)?;
    // SAFETY: both paths are NUL-terminated and live for the call. The flag
    // follows `/proc/self/fd/N` to the open file, so the new entry names that
    // inode rather than the proc symlink.
    let rc = unsafe {
        libc::linkat(
            libc::AT_FDCWD,
            fd_path.as_ptr(),
            libc::AT_FDCWD,
            dest.as_ptr(),
            libc::AT_SYMLINK_FOLLOW,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Publish the inode behind `file` as `dest`.
///
/// Prefer a hard link of the open file: that entry cannot be some other
/// attempt's staging path. If the platform cannot link an open file, rename
/// the staging entry only while it is still this inode.
fn publish_verified(file: &File, staged: &Path, dest: &Path) -> io::Result<()> {
    let parent = dest.parent().filter(|dir| !dir.as_os_str().is_empty());
    let parent = parent.unwrap_or(Path::new("."));
    let mut link_name = OsString::from(".");
    link_name.push(dest.file_name().unwrap_or(OsStr::new("update")));
    link_name.push(".publish.");
    link_name.push(attempt_token());
    let link_path = parent.join(link_name);
    if link_open_file(file, &link_path).is_ok() && same_inode(file, &link_path) {
        return std::fs::rename(&link_path, dest).inspect_err(|_| {
            remove_file_if_same(file, &link_path);
        });
    }
    remove_file_if_same(file, &link_path);
    if !same_inode(file, staged) {
        return Err(io::Error::other(
            "the verified download no longer matches its staging file",
        ));
    }
    std::fs::rename(staged, dest)
}

/// Download and verify `update` into the Downloads folder.
fn save(
    source: &impl BundleSource,
    update: &UpdateBundle,
    downloads: &Path,
) -> Result<PathBuf, String> {
    let bundle = file_name_of(&update.bundle)?;
    let mode = if bundle.ends_with(".AppImage") {
        0o755
    } else {
        0o644
    };
    let mut label = OsString::from(bundle);
    label.push(".part");
    let staged = download_exclusive(source, update, downloads, &label, mode)?;
    let path = downloads.join(bundle);
    staged
        .commit(&path)
        .map_err(|error| format!("could not save {}: {error}", path.display()))?;
    Ok(path)
}

/// Why a file couldn't be replaced.
#[cfg(target_os = "linux")]
enum Replace {
    /// Replacing isn't possible here; hand the update over instead.
    Blocked(String),
    Failed(String),
}

/// Replace the AppImage at `path` with the verified new one.
#[cfg(target_os = "linux")]
fn replace_file(
    source: &impl BundleSource,
    update: &UpdateBundle,
    path: &Path,
) -> Result<(), Replace> {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(Replace::Blocked(format!(
            "{} has no directory",
            path.display()
        )));
    };
    if dir.as_os_str().is_empty() {
        return Err(Replace::Blocked(format!(
            "{} has no directory",
            path.display()
        )));
    }
    if !writable(dir) {
        return Err(Replace::Blocked(format!(
            "{} isn't writable",
            dir.display()
        )));
    }
    let mut label = OsString::from(name);
    label.push(".update");
    let staged = download_exclusive(source, update, dir, &label, 0o755).map_err(Replace::Failed)?;
    staged
        .commit(path)
        .map_err(|error| Replace::Failed(format!("could not replace {}: {error}", path.display())))
}

/// Whether the user can create files in `dir`.
fn writable(dir: &Path) -> bool {
    let Ok(path) = std::ffi::CString::new(dir.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: `path` is a valid NUL-terminated string for the call's duration.
    unsafe { libc::access(path.as_ptr(), libc::W_OK) == 0 }
}

/// Start `command` once this process has exited, so the new copy never runs
/// beside the old one and its dashboard server.
pub fn relaunch_after_exit(command: &[OsString]) -> Result<(), String> {
    use std::os::unix::process::CommandExt;
    let mut relaunch = Command::new("/bin/sh");
    relaunch
        .arg("-c")
        .arg(r#"while kill -0 "$1" 2>/dev/null; do sleep 0.2; done; shift; exec "$@""#)
        .arg("orbit-relaunch")
        .arg(std::process::id().to_string())
        .args(command)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    #[cfg(target_os = "linux")]
    leave_appimage_environment(&mut relaunch);
    relaunch
        .spawn()
        .map(drop)
        .map_err(|error| format!("could not restart Orbit: {error}"))
}

/// Drop what the old AppImage's runtime set: its mount disappears when the
/// old app exits, so paths into it would break the new one.
#[cfg(target_os = "linux")]
fn leave_appimage_environment(command: &mut Command) {
    for name in ["APPDIR", "APPIMAGE", "ARGV0", "OWD"] {
        command.env_remove(name);
    }
    let Some(appdir) = std::env::var_os("APPDIR").filter(|dir| !dir.is_empty()) else {
        return;
    };
    let appdir = appdir.to_string_lossy().into_owned();
    for (name, value) in std::env::vars_os() {
        let Some(value) = value.to_str() else {
            continue;
        };
        if !value.contains(&appdir) {
            continue;
        }
        let kept: Vec<&str> = value
            .split(':')
            .filter(|entry| !entry.starts_with(&appdir))
            .collect();
        if kept.iter().all(|entry| entry.is_empty()) {
            command.env_remove(&name);
        } else {
            command.env(&name, kept.join(":"));
        }
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::OsStr;
    use std::fmt::{Display, Formatter};
    use std::io::Read;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    use super::{
        BundleSource, Outcome, UpdateBundle, attempt_token, download_exclusive, file_name_of,
        writable,
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
            .filter(|team| {
                !team.is_empty() && team.bytes().all(|byte| byte.is_ascii_alphanumeric())
            })
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
        let _lock =
            exclusive_lock(&parent.join(".orbit-desktop-update.lock")).inspect_err(|_| {
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

    /// A private temporary folder, removed with everything in it.
    struct Staging(PathBuf);

    impl Staging {
        fn new() -> Result<Self, String> {
            // A fresh directory per attempt. Deleting `orbit-desktop-update-<pid>`
            // would remove another attempt's disk image in this same process.
            let path =
                std::env::temp_dir().join(format!("orbit-desktop-update-{}", attempt_token()));
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
}
