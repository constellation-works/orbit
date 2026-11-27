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

// Linux compiles the macOS bundle-swap module too, so Linux CodeQL extraction
// analyzes it instead of skipping a production file. Only macOS calls into it.
#[path = "install/mac.rs"]
#[cfg_attr(
    not(target_os = "macos"),
    expect(dead_code, reason = "the macOS bundle swap runs only on macOS")
)]
mod mac;

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

/// A file this attempt created and still holds open.
///
/// Dropping it unlinks the staging name only when that name still refers to
/// this file. A name another attempt replaced is left alone.
struct StagedBytes {
    file: File,
    path: PathBuf,
}

impl StagedBytes {
    #[cfg_attr(
        not(target_os = "macos"),
        expect(dead_code, reason = "the macOS bundle swap runs only on macOS")
    )]
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

/// Why a file couldn't be replaced.
#[cfg(target_os = "linux")]
enum Replace {
    /// Replacing isn't possible here; hand the update over instead.
    Blocked(String),
    Failed(String),
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
#[expect(
    clippy::disallowed_methods,
    reason = "the AppImage runtime identifies its executable to the standalone updater through APPIMAGE"
)]
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
#[expect(
    clippy::disallowed_methods,
    reason = "relaunch removes the current AppImage mount from the inherited child environment"
)]
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
