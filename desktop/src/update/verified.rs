//! Connects a verified GitHub release to the shared installer.
//!
//! The installer lives in the `orbit_desktop` library so a test can drive it.
//! Hashing the download stays here, where the signed digest is known.

use std::fs::File;
use std::path::Path;

use orbit_desktop::update_install::{self, BundleSource, UpdateBundle};

use super::release::{Releases, Verified};

pub(crate) use update_install::{Install, Outcome, relaunch_after_exit};

struct VerifiedBundle<'a> {
    releases: &'a Releases,
    update: &'a Verified,
}

impl BundleSource for VerifiedBundle<'_> {
    fn download(&self, bundle: &UpdateBundle, file: &mut File) -> Result<(), String> {
        if bundle.bundle != self.update.bundle {
            return Err(format!(
                "refusing to download {} for {}",
                bundle.bundle, self.update.bundle
            ));
        }
        self.releases.download(self.update, file)
    }
}

/// Download `update` through `releases` and install it for `install`.
pub(crate) fn install(
    install: &Install,
    releases: &Releases,
    update: &Verified,
    identifier: &str,
    downloads: &Path,
) -> Result<Outcome, String> {
    let bundle = UpdateBundle {
        version: update.version.to_string(),
        bundle: update.bundle.clone(),
    };
    install.install(
        &VerifiedBundle { releases, update },
        &bundle,
        identifier,
        downloads,
    )
}
