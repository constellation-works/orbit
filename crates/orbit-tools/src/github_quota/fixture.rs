//! A substitute `gh` for the quota guard's execution-point tests in other
//! crates. Test fixtures only (`test-support`).

#![allow(
    clippy::disallowed_methods,
    reason = "compiled under test-support outside cfg(test), where the crate-root deny applies; this fixture writes a new gh script into a caller-owned temporary directory, where atomic replacement guards nothing, and its --warm-up probe runs only that script, which exits at once, so no deadline or process group is needed"
)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use orbit_common::OrbitError;

/// A `gh` script in its own directory that logs every call to `calls`, answers
/// `api rate_limit` with an empty object, and answers every other call with
/// `{}`, or with GitHub's authenticated primary rate-limit diagnostic when
/// built with `throttled`.
pub struct FixtureGh {
    directory: PathBuf,
}

impl FixtureGh {
    /// Write the script under `directory`, which must exist and outlive it.
    pub fn create(directory: &Path, throttled: bool) -> Result<Self, OrbitError> {
        let io = |error: std::io::Error| OrbitError::Io(error.to_string());
        let answer = if throttled {
            "echo 'gh: API rate limit exceeded for user ID 42. (HTTP 403)' >&2\nexit 1\n"
        } else {
            "echo '{}'\n"
        };
        let program = directory.join("gh");
        std::fs::write(
            &program,
            format!(
                "#!/bin/sh\n\
                 [ \"$1\" = --warm-up ] && exit 0\n\
                 printf '%s\\n' \"$*\" >> \"$(dirname \"$0\")/calls\"\n\
                 [ \"$*\" = 'api rate_limit' ] && {{ echo '{{}}'; exit 0; }}\n\
                 {answer}"
            ),
        )
        .map_err(io)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700))
                .map_err(io)?;
        }
        // Another thread forking while the script was open for writing makes
        // exec fail with ETXTBSY until that child execs; wait it out.
        let mut attempts = 0;
        while std::process::Command::new(&program)
            .arg("--warm-up")
            .status()
            .is_err()
        {
            attempts += 1;
            if attempts >= 200 {
                return Err(OrbitError::Execution(
                    "fixture gh never became executable".into(),
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(Self {
            directory: directory.to_path_buf(),
        })
    }

    /// The script's absolute path, to run in place of `gh`.
    pub fn program(&self) -> String {
        self.directory.join("gh").to_string_lossy().into_owned()
    }

    /// Every argv the script received, one line each.
    pub fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.directory.join("calls"))
            .unwrap_or_default()
            .lines()
            .map(ToString::to_string)
            .collect()
    }
}
