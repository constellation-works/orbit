use std::ffi::OsStr;
use std::fs::File;
use std::io::Read;
use std::path::Path;

#[cfg(unix)]
use std::ffi::{CString, OsString};
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd};
#[cfg(unix)]
use std::os::unix::ffi::{OsStrExt, OsStringExt};

use serde::Deserialize;

use orbit_common::OrbitError;

const DEFAULT_DOC_ROOT: &str = "docs/";
const CONFIG_FILE_NAME: &str = "config.toml";

#[derive(Debug, Deserialize)]
struct DocsConfigFile {
    docs: Option<DocsConfigSection>,
}

#[derive(Debug, Deserialize)]
struct DocsConfigSection {
    roots: Option<Vec<RawDocsRoot>>,
    search: Option<DocsSearchConfigSection>,
}

/// One `[docs] roots` entry. Either the plain path string (gitignore still
/// filters candidates under it, today's behavior) or a table naming the path
/// explicitly as authoritative over gitignore.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawDocsRoot {
    Plain(String),
    Explicit {
        path: String,
        #[serde(default = "default_respect_gitignore")]
        respect_gitignore: bool,
    },
}

fn default_respect_gitignore() -> bool {
    true
}

impl From<RawDocsRoot> for DocsRoot {
    fn from(raw: RawDocsRoot) -> Self {
        match raw {
            RawDocsRoot::Plain(path) => DocsRoot {
                path,
                respect_gitignore: true,
            },
            RawDocsRoot::Explicit {
                path,
                respect_gitignore,
            } => DocsRoot {
                path,
                respect_gitignore,
            },
        }
    }
}

/// A configured `[docs].roots` entry, resolved to whether gitignore still
/// filters candidates found beneath it. Naming a root explicitly as a table
/// (`{ path = "...", respect_gitignore = false }`) opts it out of the
/// gitignore filter; a plain string keeps today's behavior.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocsRoot {
    pub path: String,
    pub respect_gitignore: bool,
}

impl DocsRoot {
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            respect_gitignore: true,
        }
    }
}

impl From<&str> for DocsRoot {
    fn from(path: &str) -> Self {
        DocsRoot::new(path)
    }
}

impl From<String> for DocsRoot {
    fn from(path: String) -> Self {
        DocsRoot::new(path)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DocsSearchConfig {
    pub semantic_weight: f32,
}

impl Default for DocsSearchConfig {
    fn default() -> Self {
        Self {
            semantic_weight: 0.5,
        }
    }
}

#[derive(Debug, Deserialize)]
struct DocsSearchConfigSection {
    semantic_weight: Option<f32>,
}

pub fn parse_docs_roots_from_config_toml(raw: &str) -> Result<Vec<DocsRoot>, OrbitError> {
    if raw.trim().is_empty() {
        return Ok(default_doc_roots());
    }
    let parsed = toml::from_str::<DocsConfigFile>(raw).map_err(|error| {
        OrbitError::InvalidInput(format!("invalid docs config in config.toml: {error}"))
    })?;
    Ok(parsed
        .docs
        .and_then(|section| section.roots)
        .map(|roots| roots.into_iter().map(DocsRoot::from).collect())
        .unwrap_or_else(default_doc_roots))
}

pub fn parse_docs_search_config_from_config_toml(
    raw: &str,
) -> Result<DocsSearchConfig, OrbitError> {
    if raw.trim().is_empty() {
        return Ok(DocsSearchConfig::default());
    }
    let parsed = toml::from_str::<DocsConfigFile>(raw).map_err(|error| {
        OrbitError::InvalidInput(format!("invalid docs config in config.toml: {error}"))
    })?;
    let semantic_weight = parsed
        .docs
        .and_then(|section| section.search)
        .and_then(|section| section.semantic_weight)
        .unwrap_or_else(|| DocsSearchConfig::default().semantic_weight)
        .clamp(0.0, 1.0);
    Ok(DocsSearchConfig { semantic_weight })
}

pub(super) fn read_docs_roots_from_config_path(path: &Path) -> Result<Vec<DocsRoot>, OrbitError> {
    let Some(raw) = read_config_contents(path)? else {
        return Ok(default_doc_roots());
    };
    parse_docs_roots_from_config_toml(&raw)
}

pub(super) fn read_docs_search_config_from_config_path(
    path: &Path,
) -> Result<DocsSearchConfig, OrbitError> {
    let Some(raw) = read_config_contents(path)? else {
        return Ok(DocsSearchConfig::default());
    };
    parse_docs_search_config_from_config_toml(&raw)
}

pub(super) fn read_task_context_docs_roots_from_config_path(
    path: &Path,
) -> Result<Vec<DocsRoot>, OrbitError> {
    let Some(raw) = read_config_contents(path)? else {
        return Ok(default_doc_roots());
    };
    parse_task_context_docs_roots_from_config_toml(&raw)
}

fn read_config_contents(path: &Path) -> Result<Option<String>, OrbitError> {
    let Some(mut file) = open_docs_config(path)? else {
        return Ok(None);
    };

    let mut raw = String::new();
    file.read_to_string(&mut raw)
        .map_err(|error| OrbitError::Io(format!("read {}: {error}", path.display())))?;
    Ok(Some(raw))
}

fn docs_config_parent(path: &Path) -> Result<&Path, OrbitError> {
    if path.file_name() != Some(OsStr::new(CONFIG_FILE_NAME)) {
        return Err(OrbitError::InvalidInput(format!(
            "docs config path must name {CONFIG_FILE_NAME}: {}",
            path.display()
        )));
    }
    let parent = path.parent().ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "invalid config path without parent: {}",
            path.display()
        ))
    })?;

    Ok(parent)
}

/// Open the fixed config leaf relative to a held descriptor for its resolved parent.
///
/// Existing directory aliases are deliberately resolved before the descriptor is
/// opened. Once open, that directory inode is the authority: renaming an ancestor
/// cannot redirect the leaf open. `O_NOFOLLOW` makes the final `config.toml` lookup
/// atomic with opening it, while `O_NONBLOCK` prevents a planted FIFO from hanging
/// the reader before its file type can be rejected. This does not promise that a
/// caller-controlled parent chosen before this function is itself trustworthy.
#[cfg(unix)]
fn open_docs_config(path: &Path) -> Result<Option<File>, OrbitError> {
    let parent = docs_config_parent(path)?;
    let expected_parent = match parent.metadata() {
        Ok(metadata) if metadata.is_dir() => metadata,
        Ok(_) => {
            return Err(OrbitError::InvalidInput(format!(
                "docs config parent must be a directory: {}",
                parent.display()
            )));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(OrbitError::Io(format!(
                "inspect docs config directory '{}': {error}",
                parent.display()
            )));
        }
    };
    let canonical_parent = match parent.canonicalize() {
        Ok(parent) => parent,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(OrbitError::Io(format!(
                "canonicalize docs config directory '{}': {error}",
                parent.display()
            )));
        }
    };

    #[cfg(test)]
    docs_config_test_hook::run(
        docs_config_test_hook::Phase::ParentValidated,
        &canonical_parent,
    );

    let directory = open_docs_config_directory(&canonical_parent)?;
    let opened_parent = directory.metadata().map_err(|error| {
        OrbitError::Io(format!(
            "inspect opened docs config directory '{}': {error}",
            canonical_parent.display()
        ))
    })?;
    use std::os::unix::fs::MetadataExt;
    if expected_parent.dev() != opened_parent.dev() || expected_parent.ino() != opened_parent.ino()
    {
        return Err(OrbitError::InvalidInput(format!(
            "docs config directory changed while it was opened: {}",
            parent.display()
        )));
    }

    #[cfg(test)]
    docs_config_test_hook::run(docs_config_test_hook::Phase::BeforeLeafOpen, path);

    open_docs_config_at(&directory, path)
}

#[cfg(unix)]
fn open_docs_config_directory(path: &Path) -> Result<File, OrbitError> {
    let path_c = CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        OrbitError::InvalidInput(format!(
            "docs config directory contains an invalid null byte: {}",
            path.display()
        ))
    })?;
    let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW;
    let fd = unsafe { libc::open(path_c.as_ptr(), flags) };
    if fd < 0 {
        let error = std::io::Error::last_os_error();
        if matches!(error.raw_os_error(), Some(code) if code == libc::ELOOP || code == libc::ENOTDIR)
        {
            return Err(OrbitError::InvalidInput(format!(
                "docs config directory changed or became a symlink while it was opened: {}",
                path.display()
            )));
        }
        return Err(OrbitError::Io(format!(
            "open docs config directory '{}': {error}",
            path.display()
        )));
    }

    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(unix)]
fn open_docs_config_at(directory: &File, path: &Path) -> Result<Option<File>, OrbitError> {
    let file_name = c"config.toml";
    let flags = libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK;
    let fd = unsafe { libc::openat(directory.as_raw_fd(), file_name.as_ptr(), flags) };
    if fd < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::NotFound {
            return Ok(None);
        }
        if error.raw_os_error() == Some(libc::ELOOP) {
            let destination = read_docs_config_symlink_target(directory)
                .map(|target| format!(" -> {}", target.to_string_lossy()))
                .unwrap_or_default();
            return Err(OrbitError::InvalidInput(format!(
                "docs config path must not be a symlink: {}{destination}",
                path.display()
            )));
        }
        return Err(OrbitError::Io(format!(
            "open docs config '{}': {error}",
            path.display()
        )));
    }

    let file = unsafe { File::from_raw_fd(fd) };
    let metadata = file.metadata().map_err(|error| {
        OrbitError::Io(format!("inspect docs config '{}': {error}", path.display()))
    })?;
    if !metadata.is_file() {
        return Err(OrbitError::InvalidInput(format!(
            "docs config path must be a regular {CONFIG_FILE_NAME} file: {}",
            path.display()
        )));
    }

    Ok(Some(file))
}

#[cfg(unix)]
fn read_docs_config_symlink_target(directory: &File) -> Option<OsString> {
    let file_name = c"config.toml";
    let mut bytes = vec![0_u8; 256];
    loop {
        let length = unsafe {
            libc::readlinkat(
                directory.as_raw_fd(),
                file_name.as_ptr(),
                bytes.as_mut_ptr().cast(),
                bytes.len(),
            )
        };
        if length < 0 {
            return None;
        }
        let length = usize::try_from(length).ok()?;
        if length < bytes.len() {
            bytes.truncate(length);
            return Some(OsString::from_vec(bytes));
        }
        if bytes.len() >= 65_536 {
            return None;
        }
        bytes.resize(bytes.len() * 2, 0);
    }
}

/// Non-Unix platforms lack the descriptor-relative no-follow primitive used
/// above. Reject an initially visible symlink or non-regular leaf, then open it
/// read-only. A concurrent replacement between inspection and open can still be
/// followed on these platforms; callers that require the Unix race guarantee
/// must not treat this fallback as an authority boundary.
#[cfg(not(unix))]
fn open_docs_config(path: &Path) -> Result<Option<File>, OrbitError> {
    docs_config_parent(path)?;
    match path.symlink_metadata() {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(OrbitError::InvalidInput(
            format!("docs config path must not be a symlink: {}", path.display()),
        )),
        Ok(metadata) if !metadata.is_file() => Err(OrbitError::InvalidInput(format!(
            "docs config path must be a regular {CONFIG_FILE_NAME} file: {}",
            path.display()
        ))),
        Ok(_) => File::open(path).map(Some).map_err(|error| {
            OrbitError::Io(format!("open docs config '{}': {error}", path.display()))
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(OrbitError::Io(format!(
            "inspect docs config '{}': {error}",
            path.display()
        ))),
    }
}

#[cfg(all(test, unix))]
pub(super) mod docs_config_test_hook {
    use std::cell::RefCell;
    use std::path::Path;

    #[derive(Clone, Copy, PartialEq, Eq)]
    pub(crate) enum Phase {
        ParentValidated,
        BeforeLeafOpen,
    }

    type Hook = Box<dyn FnOnce(&Path)>;

    thread_local! {
        static HOOK: RefCell<Option<(Phase, Hook)>> = RefCell::new(None);
    }

    pub(crate) fn set(phase: Phase, hook: impl FnOnce(&Path) + 'static) {
        HOOK.with(|slot| *slot.borrow_mut() = Some((phase, Box::new(hook))));
    }

    pub(super) fn run(phase: Phase, path: &Path) {
        HOOK.with(|slot| {
            let should_run = slot
                .borrow()
                .as_ref()
                .is_some_and(|(hook_phase, _)| *hook_phase == phase);
            if should_run {
                let (_, hook) = slot.borrow_mut().take().expect("hook checked as present");
                hook(path);
            }
        });
    }
}

/// Parse the task-context docs roots (used by related_docs_for_task and its tests).
/// Visibility widened to pub(super) for ORB-00250 sibling tests/config.rs
/// (and the read_ wrapper in mod.rs calls it).
pub(super) fn parse_task_context_docs_roots_from_config_toml(
    raw: &str,
) -> Result<Vec<DocsRoot>, OrbitError> {
    if raw.trim().is_empty() {
        return Ok(default_doc_roots());
    }
    let parsed = toml::from_str::<DocsConfigFile>(raw).map_err(|error| {
        OrbitError::InvalidInput(format!("invalid docs config in config.toml: {error}"))
    })?;
    Ok(match parsed.docs {
        Some(section) => section
            .roots
            .map(|roots| roots.into_iter().map(DocsRoot::from).collect())
            .unwrap_or_default(),
        None => default_doc_roots(),
    })
}

fn default_doc_roots() -> Vec<DocsRoot> {
    vec![DocsRoot::new(DEFAULT_DOC_ROOT)]
}
