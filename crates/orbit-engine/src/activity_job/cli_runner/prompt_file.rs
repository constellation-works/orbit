//! Prompt delivery through a private file, for a provider CLI that cannot
//! read its prompt from stdin. [ORB-16577]

use std::io::Write;
use std::path::Path;

use tempfile::TempPath;

use super::super::dispatcher::DispatchError;

/// A prompt written to a private regular file. `TempPath` deletes the file
/// when this value drops, so it lives exactly as long as the invocation that
/// holds it, whatever that invocation's outcome.
pub(super) struct PromptFile {
    _path: TempPath,
    argument: String,
}

impl PromptFile {
    /// Write `prompt` to a new file in `scratch`, the run's own scratch
    /// directory. `tempfile` creates it with mode 0600 and an unpredictable
    /// name, so no other user can read the prompt or plant the path first.
    pub(super) fn write(scratch: &Path, prompt: &[u8]) -> Result<Self, DispatchError> {
        let failed = |error: std::io::Error| {
            DispatchError::CliInvocationPermanent(format!("write provider prompt file: {error}"))
        };
        let mut file = tempfile::Builder::new()
            .prefix("prompt-")
            .suffix(".txt")
            .tempfile_in(scratch)
            .map_err(failed)?;
        file.write_all(prompt).map_err(failed)?;
        file.flush().map_err(failed)?;
        let path = file.into_temp_path();
        let argument = path
            .to_str()
            .ok_or_else(|| {
                DispatchError::CliInvocationPermanent(format!(
                    "provider prompt file path is not UTF-8: {}",
                    path.display()
                ))
            })?
            .to_string();
        Ok(Self {
            _path: path,
            argument,
        })
    }

    /// The file's absolute path, as the provider's argv value.
    pub(super) fn argument(&self) -> &str {
        &self.argument
    }
}
