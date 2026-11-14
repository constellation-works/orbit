//! Shared guards for reading untrusted archives.

use std::io::{self, Read};
use std::path::{Component, Path};

/// Return the reason a member path must be refused, if it can escape the root.
pub fn member_path_refusal(path: &Path) -> Option<&'static str> {
    if path.components().next().is_none() {
        return Some("has an empty path");
    }
    for component in path.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir => return Some("traverses out of the archive root with `..`"),
            Component::RootDir | Component::Prefix(_) => return Some("is an absolute path"),
        }
    }
    None
}

/// A reader that fails after the stream passes its limit.
///
/// One byte of headroom preserves the existing exact-limit behavior.
pub struct BoundedReader<R> {
    inner: R,
    remaining: u64,
    message: String,
}

impl<R: Read> BoundedReader<R> {
    /// Wrap a reader with a byte limit and its over-limit error message.
    pub fn new(inner: R, limit: u64, message: String) -> Self {
        Self {
            inner,
            remaining: limit.saturating_add(1),
            message,
        }
    }
}

impl<R: Read> Read for BoundedReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let capacity = usize::try_from(self.remaining)
            .unwrap_or(usize::MAX)
            .min(buf.len());
        if capacity == 0 {
            return Err(io::Error::other(self.message.clone()));
        }
        let read = self.inner.read(&mut buf[..capacity])?;
        self.remaining -= read as u64;
        Ok(read)
    }
}
