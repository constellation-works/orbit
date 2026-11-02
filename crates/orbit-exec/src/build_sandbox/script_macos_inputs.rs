//! Bounded, pre-spawn input preparation for Seatbelt, which cannot bind inputs.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;
use std::time::{Duration, Instant};

use orbit_common::OrbitError;

use super::script::ScriptSandboxRequest;

struct InputCopyBudget {
    remaining: u64,
    started: Instant,
    timeout: Duration,
}

impl InputCopyBudget {
    fn check_deadline(&self) -> io::Result<()> {
        if self.started.elapsed() >= self.timeout {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "script input preparation timed out; nothing was run",
            ));
        }
        Ok(())
    }

    fn charge(&mut self, bytes: u64) -> io::Result<()> {
        self.check_deadline()?;
        self.remaining = self.remaining.checked_sub(bytes).ok_or_else(|| {
            io::Error::other("script inputs exceed the remaining scratch cap; nothing was run")
        })?;
        Ok(())
    }
}

/// Refuse known oversized sets before creating any input, then account for
/// every byte actually copied so growth after admission cannot bypass the cap.
/// Like the runtime size poll, count regular-file lengths without following
/// symlinks. Existing scratch files (including the snapshot) consume the budget.
/// Preparation gets its own `request.timeout`, checked per entry and chunk.
/// This is cooperative cancellation between filesystem calls, not interruption
/// of a kernel filesystem call already in progress. No copy worker is detached.
pub(super) fn copy_script_inputs(request: &ScriptSandboxRequest<'_>) -> Result<(), OrbitError> {
    let spec = &request.sandbox;
    let prepare = || -> io::Result<()> {
        let mut budget = InputCopyBudget {
            remaining: request.scratch_cap_bytes,
            started: Instant::now(),
            timeout: request.timeout,
        };
        // Resolve the scratch root itself, but do not follow links inside it,
        // matching the runtime size poll even when the caller used an alias.
        charge_tree_size(&spec.scratch.canonicalize()?, &mut budget)?;
        let remaining = budget.remaining;
        let in_dir = spec.scratch.join("in");
        for input in spec.inputs {
            budget.check_deadline()?;
            if !super::script::valid_input_name(&input.name) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("script input name `{}` is not a plain name", input.name),
                ));
            }
            if std::fs::symlink_metadata(in_dir.join(&input.name)).is_ok() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!(
                        "script input `{}` already has an entry in {}",
                        input.name,
                        in_dir.display()
                    ),
                ));
            }
            charge_tree_size(&input.source, &mut budget)
                .map_err(|error| input_preparation_error(&input.source, error))?;
        }
        // Metadata admission is only an estimate: use the original remaining
        // capacity for the streamed copy, charging actual writes, not that estimate.
        budget.remaining = remaining;
        for input in spec.inputs {
            copy_input_tree(&input.source, &in_dir.join(&input.name), &mut budget)
                .map_err(|error| input_preparation_error(&input.source, error))?;
        }
        budget.check_deadline()
    };
    prepare()
        .map_err(|error| OrbitError::InvalidInput(format!("cannot prepare script inputs: {error}")))
}

fn input_preparation_error(source: &Path, error: io::Error) -> io::Error {
    io::Error::new(error.kind(), format!("{}: {error}", source.display()))
}

fn charge_tree_size(source: &Path, budget: &mut InputCopyBudget) -> io::Result<()> {
    budget.check_deadline()?;
    let metadata = std::fs::symlink_metadata(source)?;
    if metadata.is_dir() {
        for entry in std::fs::read_dir(source)? {
            budget.check_deadline()?;
            charge_tree_size(&entry?.path(), budget)?;
        }
    } else if metadata.is_file() {
        budget.charge(metadata.len())?;
    } else if !metadata.is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a file, directory or symlink", source.display()),
        ));
    }
    Ok(())
}

fn copy_input_tree(
    source: &Path,
    destination: &Path,
    budget: &mut InputCopyBudget,
) -> io::Result<()> {
    budget.check_deadline()?;
    let metadata = std::fs::symlink_metadata(source)?;
    let kind = metadata.file_type();
    if kind.is_dir() {
        // No eager collection/sort: even enumerating a large input must check
        // the deadline between entries, before allocating more scratch space.
        std::fs::DirBuilder::new().mode(0o700).create(destination)?;
        for entry in std::fs::read_dir(source)? {
            budget.check_deadline()?;
            let entry = entry?;
            copy_input_tree(&entry.path(), &destination.join(entry.file_name()), budget)?;
        }
        Ok(())
    } else if kind.is_file() {
        copy_input_file(source, destination, budget)
    } else if kind.is_symlink() {
        std::os::unix::fs::symlink(std::fs::read_link(source)?, destination)
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a file, directory or symlink", source.display()),
        ))
    }
}

fn copy_input_file(
    source: &Path,
    destination: &Path,
    budget: &mut InputCopyBudget,
) -> io::Result<()> {
    // A source replaced by a FIFO must not block open, and a symlink swap
    // must not turn a copied file into an unadmitted read of its target.
    let mut input = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(source)?;
    let metadata = input.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is no longer a regular file", source.display()),
        ));
    }
    let mut output: File = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(destination)?;
    let mut buffer = [0u8; 8192];
    loop {
        budget.check_deadline()?;
        // Read at most one byte beyond the remaining cap to detect growth
        // without reading the rest of an oversized file. Never write that byte.
        let read_limit = budget.remaining.saturating_add(1).min(buffer.len() as u64) as usize;
        let count = input.read(&mut buffer[..read_limit])?;
        if count == 0 {
            break;
        }
        budget.charge(count as u64)?;
        output.write_all(&buffer[..count])?;
        #[cfg(test)]
        AFTER_INPUT_COPY_CHUNK.with(|hook| {
            if let Some(hook) = hook.take() {
                hook();
            }
        });
    }
    output.set_permissions(metadata.permissions())?;
    budget.check_deadline()
}

// A one-shot, thread-local fixture grows an already-open regular file at
// an exact chunk boundary. It is absent from shipped builds.
#[cfg(test)]
thread_local! {
    pub(super) static AFTER_INPUT_COPY_CHUNK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}
