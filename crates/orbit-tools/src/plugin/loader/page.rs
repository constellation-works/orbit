//! `spec.web.pages[]`: read, check and hash each page's files (plugin-pages
//! design `docs/design/plugin-pages/2_design.md` §1).
//!
//! A page is `index.html` plus the scripts and styles its manifest lists,
//! all under its `root`. Orbit later writes those files into one document,
//! so a file that would end its element early, or markup that holds its own
//! `script` or `link` element, is refused here. Files in `root` the manifest
//! does not list are never opened.

use orbit_types::identity::lower_hex;
use orbit_types::plugin::{PAGE_INDEX_FILE, PluginWebPage};

use super::*;

/// The most bytes one page may hold: `index.html` plus every listed script
/// and style, as read from disk (2 MiB).
pub const MAX_PLUGIN_PAGE_BYTES: u64 = 2 * 1024 * 1024;

/// Why one page cannot be written out. Every message names the page; the
/// strict loader reports it as the refusal of the plugin, and the serving
/// loader keeps it on that page.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PluginPageError {
    #[error(
        "page '{page}': root '{root}' is a symbolic link; a page root must be a directory \
         inside the plugin root"
    )]
    RootSymlink { page: String, root: String },
    #[error("page '{page}': root '{root}' is not a directory")]
    RootNotDirectory { page: String, root: String },
    #[error("page '{page}': root '{root}' cannot be read: {reason}")]
    RootUnreadable {
        page: String,
        root: String,
        reason: String,
    },
    #[error(
        "page '{page}': root '{root}' resolves to {resolved} outside the plugin root {plugin_root}"
    )]
    RootOutsidePluginRoot {
        page: String,
        root: String,
        resolved: String,
        plugin_root: String,
    },
    #[error("page '{page}': '{file}' is missing from '{root}'")]
    FileMissing {
        page: String,
        root: String,
        file: String,
    },
    #[error("page '{page}': '{file}' cannot be read: {reason}")]
    FileUnreadable {
        page: String,
        file: String,
        reason: String,
    },
    #[error("page '{page}': '{file}' is a symbolic link; a page file must be a regular file")]
    FileSymlink { page: String, file: String },
    #[error("page '{page}': '{file}' is not a regular file")]
    FileNotRegular { page: String, file: String },
    #[error("page '{page}': '{file}' resolves to {resolved} outside the plugin root {plugin_root}")]
    FileOutsidePluginRoot {
        page: String,
        file: String,
        resolved: String,
        plugin_root: String,
    },
    #[error("page '{page}' lists '{file}' more than once")]
    FileListedTwice { page: String, file: String },
    #[error(
        "page '{page}': index.html holds a `{element}` element; Orbit writes the page's \
         scripts and styles itself, so list them under `scripts` and `styles`"
    )]
    MarkupElement { page: String, element: String },
    #[error(
        "page '{page}': script '{file}' contains `{found}`, which would end its `script` \
         element early"
    )]
    ScriptEndsEarly {
        page: String,
        file: String,
        found: String,
    },
    #[error(
        "page '{page}': style '{file}' contains `{found}`, which would end its `style` \
         element early"
    )]
    StyleEndsEarly {
        page: String,
        file: String,
        found: String,
    },
    #[error(
        "page '{page}' holds at least {total} bytes; a page may hold at most {} bytes (2 MiB) \
         across index.html and its listed files",
        MAX_PLUGIN_PAGE_BYTES
    )]
    TooLarge { page: String, total: u64 },
}

/// One `spec.web.pages[]` entry as this load found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedPluginPage {
    pub id: String,
    /// The sub-tab label: the manifest title, or the id when it is blank.
    pub title: String,
    /// Verbs of this plugin's `read_only` tools the page may call.
    pub tools: Vec<String>,
    /// The bridge version the page was written for, when it names one.
    pub kit: Option<u32>,
    /// The page's files and digest, or why they could not be read. The
    /// strict loader refuses the plugin for an `Err`; the serving loader
    /// keeps it, so one page that fails leaves the rest of the plugin up.
    pub bundle: Result<PluginPageBundle, PluginPageError>,
}

impl LoadedPluginPage {
    /// The digest of the files this load read, when it read them.
    pub fn digest(&self) -> Option<&str> {
        self.bundle
            .as_ref()
            .ok()
            .map(|bundle| bundle.digest.as_str())
    }
}

/// The bytes of one page, kept in memory so the bytes served are the bytes
/// that were hashed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginPageBundle {
    /// `index.html`.
    pub markup: Vec<u8>,
    /// The listed scripts, in manifest order.
    pub scripts: Vec<PluginPageFile>,
    /// The listed styles, in manifest order.
    pub styles: Vec<PluginPageFile>,
    /// SHA-256 over every file above, hex encoded. See [`page_digest`].
    pub digest: String,
}

/// One listed page file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginPageFile {
    /// The name as the manifest lists it, relative to the page root.
    pub name: String,
    pub bytes: Vec<u8>,
}

/// Read every declared page. `strict` turns the first page that fails into
/// a refusal of the plugin, naming the page.
pub(super) fn resolve_pages(
    root: &Path,
    manifest: &PluginManifest,
    strict: bool,
) -> Result<Vec<LoadedPluginPage>, PluginLoadError> {
    let Some(web) = manifest.spec.web.as_ref() else {
        return Ok(Vec::new());
    };
    let mut pages = Vec::with_capacity(web.pages.len());
    for (index, page) in web.pages.iter().enumerate() {
        let bundle = load_page_bundle(root, page);
        if strict && let Err(error) = &bundle {
            return Err(PluginManifestError::new(
                format!("spec.web.pages[{index}]"),
                error.to_string(),
            )
            .into());
        }
        pages.push(LoadedPluginPage {
            id: page.id.clone(),
            title: page.label().to_string(),
            tools: page.tools.clone(),
            kit: page.kit,
            bundle,
        });
    }
    Ok(pages)
}

/// Why a page is not served, or `None` when its files hash to the digest
/// this host recorded at install. `recorded` is that digest, absent when
/// the install recorded none for this page id.
///
/// The recovery is the forced reinstall a changed manifest needs: install
/// re-records the digest of the files it copies.
pub fn page_digest_diagnostic(
    namespace: &str,
    recorded: Option<&str>,
    page: &LoadedPluginPage,
) -> Option<String> {
    let id = &page.id;
    let found = match &page.bundle {
        Ok(bundle) => bundle.digest.as_str(),
        Err(error) => {
            return Some(format!(
                "{error}; reinstall plugin '{namespace}' with `orbit plugin add --force`"
            ));
        }
    };
    match recorded {
        Some(recorded) if recorded == found => None,
        Some(recorded) => Some(format!(
            "page '{id}' of plugin '{namespace}': its files hash to {found}, but this host \
             recorded {recorded} at install; reinstall with `orbit plugin add --force` to \
             serve the files on disk"
        )),
        None => Some(format!(
            "page '{id}' of plugin '{namespace}': its files hash to {found}, but this host \
             recorded no digest for it at install; reinstall with `orbit plugin add --force`"
        )),
    }
}

/// Whether every page of `plugin` still reads to the bundle it holds. A
/// long-lived host uses this before reusing a cached load, so a page file
/// edited after the first load is seen on the next pass.
pub fn plugin_pages_unchanged(plugin: &LoadedPlugin) -> bool {
    let Some(web) = plugin.manifest.spec.web.as_ref() else {
        return plugin.pages.is_empty();
    };
    web.pages.len() == plugin.pages.len()
        && web.pages.iter().zip(&plugin.pages).all(|(page, loaded)| {
            let fresh = load_page_bundle(&plugin.root, page);
            match (&fresh, &loaded.bundle) {
                (Ok(fresh), Ok(loaded)) => fresh.digest == loaded.digest,
                (Err(fresh), Err(loaded)) => fresh == loaded,
                _ => false,
            }
        })
}

/// SHA-256 over a page's files in a fixed framing: for `index.html`, then
/// each script and each style in manifest order, the part's kind, its name,
/// its length and its bytes. Renaming, reordering or moving a file between
/// lists changes the digest, as editing it does.
pub fn page_digest(bundle_parts: &[(&str, &str, &[u8])]) -> String {
    let mut hasher = Sha256::new();
    for (kind, name, bytes) in bundle_parts {
        for field in [kind.as_bytes(), name.as_bytes()] {
            hasher.update((field.len() as u64).to_le_bytes());
            hasher.update(field);
        }
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    }
    lower_hex(hasher.finalize())
}

fn load_page_bundle(
    root: &Path,
    page: &PluginWebPage,
) -> Result<PluginPageBundle, PluginPageError> {
    let id = &page.id;
    let declared_root = page.root.trim();
    let page_root = root.join(declared_root);
    let root_unreadable = |reason: String| PluginPageError::RootUnreadable {
        page: id.clone(),
        root: declared_root.to_string(),
        reason,
    };
    match std::fs::symlink_metadata(&page_root) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(PluginPageError::RootSymlink {
                page: id.clone(),
                root: declared_root.to_string(),
            });
        }
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => {
            return Err(PluginPageError::RootNotDirectory {
                page: id.clone(),
                root: declared_root.to_string(),
            });
        }
        Err(error) => return Err(root_unreadable(error.to_string())),
    }
    let page_root =
        std::fs::canonicalize(&page_root).map_err(|error| root_unreadable(error.to_string()))?;
    if !page_root.starts_with(root) {
        return Err(PluginPageError::RootOutsidePluginRoot {
            page: id.clone(),
            root: declared_root.to_string(),
            resolved: page_root.display().to_string(),
            plugin_root: root.display().to_string(),
        });
    }

    let mut seen = std::collections::BTreeSet::new();
    let mut total: u64 = 0;
    let markup = read_page_file(
        root,
        &page_root,
        page,
        PAGE_INDEX_FILE,
        &mut seen,
        &mut total,
    )?;
    if let Some(element) = find_any_ascii_ci(&markup, &["<script", "<link"]) {
        return Err(PluginPageError::MarkupElement {
            page: id.clone(),
            element: element.to_string(),
        });
    }
    let mut scripts = Vec::with_capacity(page.scripts.len());
    for name in &page.scripts {
        let name = name.trim();
        let bytes = read_page_file(root, &page_root, page, name, &mut seen, &mut total)?;
        if let Some(found) = find_any_ascii_ci(&bytes, &["</script", "<!--"]) {
            return Err(PluginPageError::ScriptEndsEarly {
                page: id.clone(),
                file: name.to_string(),
                found: found.to_string(),
            });
        }
        scripts.push(PluginPageFile {
            name: name.to_string(),
            bytes,
        });
    }
    let mut styles = Vec::with_capacity(page.styles.len());
    for name in &page.styles {
        let name = name.trim();
        let bytes = read_page_file(root, &page_root, page, name, &mut seen, &mut total)?;
        if let Some(found) = find_any_ascii_ci(&bytes, &["</style"]) {
            return Err(PluginPageError::StyleEndsEarly {
                page: id.clone(),
                file: name.to_string(),
                found: found.to_string(),
            });
        }
        styles.push(PluginPageFile {
            name: name.to_string(),
            bytes,
        });
    }

    let mut parts: Vec<(&str, &str, &[u8])> = vec![("markup", PAGE_INDEX_FILE, &markup)];
    parts.extend(
        scripts
            .iter()
            .map(|file| ("script", file.name.as_str(), file.bytes.as_slice())),
    );
    parts.extend(
        styles
            .iter()
            .map(|file| ("style", file.name.as_str(), file.bytes.as_slice())),
    );
    let digest = page_digest(&parts);
    Ok(PluginPageBundle {
        markup,
        scripts,
        styles,
        digest,
    })
}

/// Read one page file: present, a regular file and not a link, inside the
/// plugin root, not already read for this page, and within the size bound
/// counted across the page so far.
fn read_page_file(
    root: &Path,
    page_root: &Path,
    page: &PluginWebPage,
    name: &str,
    seen: &mut std::collections::BTreeSet<PathBuf>,
    total: &mut u64,
) -> Result<Vec<u8>, PluginPageError> {
    let id = &page.id;
    let declared_root = page.root.trim();
    let path = page_root.join(name);
    let file_unreadable = |error: std::io::Error| PluginPageError::FileUnreadable {
        page: id.clone(),
        file: name.to_string(),
        reason: error.to_string(),
    };
    let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            PluginPageError::FileMissing {
                page: id.clone(),
                root: declared_root.to_string(),
                file: name.to_string(),
            }
        } else {
            file_unreadable(error)
        }
    })?;
    if metadata.file_type().is_symlink() {
        return Err(PluginPageError::FileSymlink {
            page: id.clone(),
            file: name.to_string(),
        });
    }
    if !metadata.is_file() {
        return Err(PluginPageError::FileNotRegular {
            page: id.clone(),
            file: name.to_string(),
        });
    }
    let resolved = std::fs::canonicalize(&path).map_err(file_unreadable)?;
    if !resolved.starts_with(root) {
        return Err(PluginPageError::FileOutsidePluginRoot {
            page: id.clone(),
            file: name.to_string(),
            resolved: resolved.display().to_string(),
            plugin_root: root.display().to_string(),
        });
    }
    if !seen.insert(resolved) {
        return Err(PluginPageError::FileListedTwice {
            page: id.clone(),
            file: name.to_string(),
        });
    }
    *total = total.saturating_add(metadata.len());
    if *total > MAX_PLUGIN_PAGE_BYTES {
        return Err(PluginPageError::TooLarge {
            page: id.clone(),
            total: *total,
        });
    }
    let bytes = std::fs::read(&path).map_err(file_unreadable)?;
    // The length is counted again from the bytes read, so a file that grew
    // between the stat and the read cannot carry the page past the bound.
    *total = total
        .saturating_sub(metadata.len())
        .saturating_add(bytes.len() as u64);
    if *total > MAX_PLUGIN_PAGE_BYTES {
        return Err(PluginPageError::TooLarge {
            page: id.clone(),
            total: *total,
        });
    }
    Ok(bytes)
}

/// The first needle found in `haystack`, compared without ASCII case.
fn find_any_ascii_ci<'a>(haystack: &[u8], needles: &[&'a str]) -> Option<&'a str> {
    needles.iter().copied().find(|needle| {
        let needle = needle.as_bytes();
        haystack
            .windows(needle.len())
            .any(|window| window.eq_ignore_ascii_case(needle))
    })
}

/// Name the page whose root holds a refused symbolic link, when one does.
///
/// The tree-wide link walk runs before the manifest is parsed. When it
/// refuses an entry, the manifest is read only to say which page the link
/// belongs to; a manifest that is itself a link, or does not parse, gets the
/// walk's own message.
pub(super) fn page_owning_entry(root: &Path, entry: &Path) -> Option<(usize, String)> {
    let manifest_path = root.join(MANIFEST_FILE_NAME);
    let metadata = std::fs::symlink_metadata(&manifest_path).ok()?;
    if !metadata.is_file() {
        return None;
    }
    let bytes = std::fs::read(&manifest_path).ok()?;
    let manifest: PluginManifest = serde_yaml::from_slice(&bytes).ok()?;
    let web = manifest.spec.web?;
    web.pages
        .iter()
        .enumerate()
        .find(|(_, page)| entry.starts_with(Path::new(page.root.trim())))
        .map(|(index, page)| (index, page.id.clone()))
}
