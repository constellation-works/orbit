//! Overlapping desktop installs cannot publish a download they did not verify.
//!
//! Two attempts share an AppImage or a Downloads file. The verified attempt
//! pauses inside its transport until the other attempt has written a partial
//! file, then commits. The partial attempt reports failure only after that
//! commit. The installed bytes must be the verified attempt's, and staging
//! names that neither attempt owns must still be there.

#[cfg(target_os = "linux")]
mod linux {
    use std::fs::File;
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, mpsc};

    use orbit_desktop::update_install::{BundleSource, Install, Outcome, UpdateBundle};

    const VERIFIED: &[u8] = b"VERIFIED-ATTEMPT-A";
    const PARTIAL: &[u8] = b"UNVERIFIED-PARTIAL-B";
    const ORIGINAL: &[u8] = b"ORIGINAL-APP";
    const LEGACY_STAGING: &[u8] = b"LEGACY-STAGING";
    const OTHER_STAGING: &[u8] = b"OTHER-ATTEMPT-STAGING";
    const UNRELATED: &[u8] = b"UNRELATED-INSTALL";
    const TRANSFER_FAILED: &str = "transfer failed";

    struct Scripted<F> {
        bytes: &'static [u8],
        coordinate: F,
        error: Option<&'static str>,
    }

    impl<F> BundleSource for Scripted<F>
    where
        F: Fn(),
    {
        fn download(&self, _update: &UpdateBundle, file: &mut File) -> Result<(), String> {
            file.write_all(self.bytes)
                .map_err(|error| error.to_string())?;
            (self.coordinate)();
            match self.error {
                Some(error) => Err(error.to_string()),
                None => Ok(()),
            }
        }
    }

    /// Sends one permit when dropped, without blocking if one is already queued.
    struct Release(Option<mpsc::SyncSender<()>>);

    impl Release {
        fn arm(sender: mpsc::SyncSender<()>) -> Self {
            Self(Some(sender))
        }
    }

    impl Drop for Release {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.try_send(());
            }
        }
    }

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Result<Self, String> {
            let path = std::env::temp_dir().join(format!(
                "orbit-desktop-update-install-{label}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::DirBuilder::new()
                .create(&path)
                .map_err(|error| format!("create {}: {error}", path.display()))?;
            Ok(Self(path))
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write_bytes(path: &Path, bytes: &[u8]) -> Result<(), String> {
        if let Some(parent) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
            std::fs::DirBuilder::new()
                .recursive(true)
                .create(parent)
                .map_err(|error| format!("create {}: {error}", parent.display()))?;
        }
        let mut file =
            File::create(path).map_err(|error| format!("create {}: {error}", path.display()))?;
        file.write_all(bytes)
            .map_err(|error| format!("write {}: {error}", path.display()))
    }

    fn read_bytes(path: &Path) -> Result<Vec<u8>, String> {
        std::fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))
    }

    fn expect_bytes(path: &Path, want: &[u8]) -> Result<(), String> {
        let got = read_bytes(path)?;
        if got != want {
            return Err(format!("{} contains {got:?}, not {want:?}", path.display()));
        }
        Ok(())
    }

    fn expect_entries(dir: &Path, names: &[&str]) -> Result<(), String> {
        let mut found = Vec::new();
        let entries =
            std::fs::read_dir(dir).map_err(|error| format!("read {}: {error}", dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|error| error.to_string())?;
            found.push(entry.file_name().to_string_lossy().into_owned());
        }
        found.sort();
        let mut want: Vec<String> = names.iter().map(|name| (*name).to_string()).collect();
        want.sort();
        if found != want {
            return Err(format!(
                "{} contains {found:?}, not the files left for this attempt: {want:?}",
                dir.display()
            ));
        }
        Ok(())
    }

    fn reject_bytes(dir: &Path, forbidden: &[u8]) -> Result<(), String> {
        let entries =
            std::fs::read_dir(dir).map_err(|error| format!("read {}: {error}", dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|error| error.to_string())?;
            let path = entry.path();
            let kind = entry
                .file_type()
                .map_err(|error| format!("stat {}: {error}", path.display()))?;
            if kind.is_dir() {
                reject_bytes(&path, forbidden)?;
            } else if kind.is_file() {
                let data = read_bytes(&path)?;
                if data == forbidden {
                    return Err(format!(
                        "{} holds the unverified partial download",
                        path.display()
                    ));
                }
            }
        }
        Ok(())
    }

    fn bundle(name: &str) -> UpdateBundle {
        UpdateBundle {
            version: "0.29.0".to_string(),
            bundle: name.to_string(),
        }
    }

    /// What each overlapping installer returned.
    struct Overlap {
        verified: Result<Outcome, String>,
        partial: Result<Outcome, String>,
    }

    /// Run the verified attempt and, once it has written, the partial one.
    fn overlap<A, B>(verified: A, partial: B) -> Result<Overlap, String>
    where
        A: FnOnce(&dyn Fn()) -> Result<Outcome, String> + Send + 'static,
        B: FnOnce(&dyn Fn()) -> Result<Outcome, String> + Send + 'static,
    {
        let (wrote_tx, wrote_rx) = mpsc::sync_channel(1);
        let (staged_tx, staged_rx) = mpsc::sync_channel(1);
        let (finished_tx, finished_rx) = mpsc::sync_channel(1);
        let verified_wrote = Arc::new(AtomicBool::new(false));
        let partial_wrote = Arc::new(AtomicBool::new(false));
        let verified_flag = Arc::clone(&verified_wrote);
        let partial_flag = Arc::clone(&partial_wrote);

        let verified_thread = std::thread::spawn(move || {
            let _release = Release::arm(wrote_tx.clone());
            verified(&|| {
                verified_flag.store(true, Ordering::SeqCst);
                let _ = wrote_tx.try_send(());
                let _ = staged_rx.recv();
            })
        });
        wrote_rx
            .recv()
            .map_err(|_| "the verified attempt ended before it wrote".to_string())?;

        let partial_thread = std::thread::spawn(move || {
            let _release = Release::arm(staged_tx.clone());
            partial(&|| {
                partial_flag.store(true, Ordering::SeqCst);
                let _ = staged_tx.try_send(());
                let _ = finished_rx.recv();
            })
        });
        let verified_result = verified_thread
            .join()
            .map_err(|_| "the verified attempt panicked".to_string())?;
        let _ = finished_tx.try_send(());
        let partial_result = partial_thread
            .join()
            .map_err(|_| "the partial attempt panicked".to_string())?;
        if !verified_wrote.load(Ordering::SeqCst) || !partial_wrote.load(Ordering::SeqCst) {
            return Err(
                "the attempts did not both write while the verified download was still open".into(),
            );
        }
        Ok(Overlap {
            verified: verified_result,
            partial: partial_result,
        })
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn overlapping_appimage_downloads_publish_only_the_verified_bytes() -> Result<(), String> {
        let root = Scratch::new("appimage")?;
        let app = root.path().join("Orbit.AppImage");
        let downloads = root.path().join("downloads");
        write_bytes(&app, ORIGINAL)?;
        write_bytes(&root.path().join(".Orbit.AppImage.update"), LEGACY_STAGING)?;
        write_bytes(
            &root.path().join(".Orbit.AppImage.update.other-attempt"),
            OTHER_STAGING,
        )?;
        write_bytes(&root.path().join("other-app").join("payload"), UNRELATED)?;
        let verified_app = app.clone();
        let partial_app = app.clone();
        let verified_downloads = downloads.clone();
        let partial_downloads = downloads.clone();
        let Overlap { verified, partial } = overlap(
            move |coordinate| {
                Install::AppImage(verified_app).install(
                    &Scripted {
                        bytes: VERIFIED,
                        coordinate,
                        error: None,
                    },
                    &bundle("Orbit.AppImage"),
                    "works.constellation.orbit.desktop",
                    &verified_downloads,
                )
            },
            move |coordinate| {
                Install::AppImage(partial_app).install(
                    &Scripted {
                        bytes: PARTIAL,
                        coordinate,
                        error: Some(TRANSFER_FAILED),
                    },
                    &bundle("Orbit.AppImage"),
                    "works.constellation.orbit.desktop",
                    &partial_downloads,
                )
            },
        )?;
        match verified {
            Ok(Outcome::Restart(command)) => {
                if command.first().map(std::ffi::OsString::as_os_str) != Some(app.as_os_str()) {
                    return Err(format!("restart command was {command:?}"));
                }
            }
            other => {
                return Err(format!(
                    "the verified AppImage install did not replace it: {other:?}"
                ));
            }
        }
        match partial {
            Err(error) if error.contains(TRANSFER_FAILED) => {}
            other => {
                return Err(format!(
                    "the partial AppImage install should fail its transfer, got {other:?}"
                ));
            }
        }
        expect_bytes(&app, VERIFIED)?;
        expect_bytes(&root.path().join(".Orbit.AppImage.update"), LEGACY_STAGING)?;
        expect_bytes(
            &root.path().join(".Orbit.AppImage.update.other-attempt"),
            OTHER_STAGING,
        )?;
        expect_bytes(&root.path().join("other-app").join("payload"), UNRELATED)?;
        expect_entries(
            root.path(),
            &[
                "Orbit.AppImage",
                ".Orbit.AppImage.update",
                ".Orbit.AppImage.update.other-attempt",
                "other-app",
            ],
        )?;
        reject_bytes(root.path(), PARTIAL)
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn overlapping_saved_downloads_publish_only_the_verified_bytes() -> Result<(), String> {
        let root = Scratch::new("save")?;
        let downloads = root.path().join("downloads");
        let saved = downloads.join("orbit.deb");
        write_bytes(&saved, ORIGINAL)?;
        write_bytes(&downloads.join(".orbit.deb.part"), LEGACY_STAGING)?;
        write_bytes(
            &downloads.join(".orbit.deb.part.other-attempt"),
            OTHER_STAGING,
        )?;
        write_bytes(&downloads.join("other-package").join("payload"), UNRELATED)?;
        let verified_downloads = downloads.clone();
        let partial_downloads = downloads.clone();
        let Overlap { verified, partial } = overlap(
            move |coordinate| {
                Install::Package.install(
                    &Scripted {
                        bytes: VERIFIED,
                        coordinate,
                        error: None,
                    },
                    &bundle("orbit.deb"),
                    "works.constellation.orbit.desktop",
                    &verified_downloads,
                )
            },
            move |coordinate| {
                Install::Package.install(
                    &Scripted {
                        bytes: PARTIAL,
                        coordinate,
                        error: Some(TRANSFER_FAILED),
                    },
                    &bundle("orbit.deb"),
                    "works.constellation.orbit.desktop",
                    &partial_downloads,
                )
            },
        )?;
        match verified {
            Ok(Outcome::Saved { path, open, .. }) => {
                if open || path != saved {
                    return Err(format!("saved {path:?} open={open}, want {saved:?}"));
                }
            }
            other => {
                return Err(format!(
                    "the verified save did not keep the download: {other:?}"
                ));
            }
        }
        match partial {
            Err(error) if error.contains(TRANSFER_FAILED) => {}
            other => {
                return Err(format!(
                    "the partial save should fail its transfer, got {other:?}"
                ));
            }
        }
        expect_bytes(&saved, VERIFIED)?;
        expect_bytes(&downloads.join(".orbit.deb.part"), LEGACY_STAGING)?;
        expect_bytes(
            &downloads.join(".orbit.deb.part.other-attempt"),
            OTHER_STAGING,
        )?;
        expect_bytes(&downloads.join("other-package").join("payload"), UNRELATED)?;
        expect_entries(
            &downloads,
            &[
                "orbit.deb",
                ".orbit.deb.part",
                ".orbit.deb.part.other-attempt",
                "other-package",
            ],
        )?;
        reject_bytes(root.path(), PARTIAL)
    }
}
