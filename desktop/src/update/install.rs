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

use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::release::{Releases, Verified};

/// How this copy of the app was installed.
pub(crate) enum Install {
    /// A macOS app bundle.
    #[cfg(target_os = "macos")]
    MacApp {
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
pub(crate) enum Outcome {
    /// The app was replaced: run this command once the app has exited.
    Restart(Vec<OsString>),
    /// The verified bundle was saved for the user to install.
    Saved {
        path: PathBuf,
        /// Open the file (a disk image) rather than reveal it.
        open: bool,
        message: String,
    },
}

impl Install {
    /// How the running app was installed. `None` for a development build or
    /// a platform without a desktop release.
    pub(crate) fn detect() -> Option<Self> {
        if tauri::is_dev() {
            return None;
        }
        detect_platform()
    }

    /// The release asset this install updates from.
    pub(crate) fn asset(&self) -> Option<&'static str> {
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
    pub(crate) fn describe(&self) -> String {
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
    pub(crate) fn install(
        &self,
        releases: &Releases,
        update: &Verified,
        identifier: &str,
        downloads: &Path,
    ) -> Result<Outcome, String> {
        match self {
            #[cfg(target_os = "macos")]
            Self::MacApp {
                bundle,
                blocked: None,
            } => mac::replace(releases, update, bundle, identifier),
            #[cfg(target_os = "macos")]
            Self::MacApp { .. } => {
                let path = save(releases, update, downloads)?;
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
                match replace_file(releases, update, path) {
                    Ok(()) => Ok(Outcome::Restart(vec![path.clone().into_os_string()])),
                    Err(Replace::Blocked(reason)) => {
                        let saved = save(releases, update, downloads)?;
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
                let path = save(releases, update, downloads)?;
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

/// Create `path` for writing, readable only by the user (or executable too).
fn create_private(path: &Path, mode: u32) -> Result<File, String> {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("could not clear {}: {error}", path.display())),
    }
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)
        .map_err(|error| format!("could not create {}: {error}", path.display()))
}

/// Download and verify `update` into `path`, removing it on failure.
fn download_to(
    releases: &Releases,
    update: &Verified,
    path: &Path,
    mode: u32,
) -> Result<(), String> {
    let mut file = create_private(path, mode)?;
    let result = releases.download(update, &mut file);
    drop(file);
    if result.is_err() {
        let _ = std::fs::remove_file(path);
    }
    result
}

/// Download and verify `update` into the Downloads folder.
fn save(releases: &Releases, update: &Verified, downloads: &Path) -> Result<PathBuf, String> {
    let partial = downloads.join(format!(".{}.part", update.bundle));
    let path = downloads.join(&update.bundle);
    let mode = if update.bundle.ends_with(".AppImage") {
        0o755
    } else {
        0o644
    };
    download_to(releases, update, &partial, mode)?;
    std::fs::rename(&partial, &path).map_err(|error| {
        let _ = std::fs::remove_file(&partial);
        format!("could not save {}: {error}", path.display())
    })?;
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
fn replace_file(releases: &Releases, update: &Verified, path: &Path) -> Result<(), Replace> {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(Replace::Blocked(format!(
            "{} has no directory",
            path.display()
        )));
    };
    if !writable(dir) {
        return Err(Replace::Blocked(format!(
            "{} isn't writable",
            dir.display()
        )));
    }
    let mut staged_name = OsString::from(".");
    staged_name.push(name);
    staged_name.push(".update");
    let staged = dir.join(staged_name);
    download_to(releases, update, &staged, 0o755).map_err(Replace::Failed)?;
    std::fs::rename(&staged, path).map_err(|error| {
        let _ = std::fs::remove_file(&staged);
        Replace::Failed(format!("could not replace {}: {error}", path.display()))
    })
}

/// Whether the user can create files in `dir`.
fn writable(dir: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(path) = std::ffi::CString::new(dir.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: `path` is a valid NUL-terminated string for the call's duration.
    unsafe { libc::access(path.as_ptr(), libc::W_OK) == 0 }
}

/// Start `command` once this process has exited, so the new copy never runs
/// beside the old one and its dashboard server.
pub(crate) fn relaunch_after_exit(command: &[OsString]) -> Result<(), String> {
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
    use std::io::Read;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    use super::{Outcome, download_to, writable};
    use crate::update::release::{Releases, Verified, Version};

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
        releases: &Releases,
        update: &Verified,
        bundle: &Path,
        identifier: &str,
    ) -> Result<Outcome, String> {
        let team = team_id(bundle).ok_or("the running app's Team ID could not be read")?;
        let staging = Staging::new()?;
        let dmg = staging.0.join(&update.bundle);
        download_to(releases, update, &dmg, 0o600)?;
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
                .arg(&dmg),
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
        if reported != Some(update.version) {
            return Err(format!(
                "the update's app reports version {}, not {}",
                reported.map_or_else(|| "nothing".into(), |version| version.to_string()),
                update.version
            ));
        }

        let parent = bundle.parent().ok_or("the app has no parent folder")?;
        let staged = parent.join(".Orbit.app.update");
        let previous = parent.join(".Orbit.app.previous");
        remove_tree(&staged)?;
        remove_tree(&previous)?;
        run(
            Command::new("/usr/bin/ditto").arg(&new_app).arg(&staged),
            SLOW,
        )
        .and_then(Captured::succeeded)
        .inspect_err(|_| {
            let _ = std::fs::remove_dir_all(&staged);
        })?;
        drop(attached);
        std::fs::rename(bundle, &previous).map_err(|error| {
            let _ = std::fs::remove_dir_all(&staged);
            format!("could not move {} aside: {error}", bundle.display())
        })?;
        if let Err(error) = std::fs::rename(&staged, bundle) {
            let _ = std::fs::rename(&previous, bundle);
            let _ = std::fs::remove_dir_all(&staged);
            return Err(format!("could not install the update: {error}"));
        }
        let _ = std::fs::remove_dir_all(&previous);
        Ok(Outcome::Restart(vec![
            "/usr/bin/open".into(),
            bundle.as_os_str().to_owned(),
        ]))
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

    fn remove_tree(path: &Path) -> Result<(), String> {
        match std::fs::remove_dir_all(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("could not clear {}: {error}", path.display())),
        }
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
            let path =
                std::env::temp_dir().join(format!("orbit-desktop-update-{}", std::process::id()));
            remove_tree(&path)?;
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
