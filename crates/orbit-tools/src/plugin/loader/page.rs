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
    pub bundle: Result<PluginPageBundle, String>,
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
        if strict && let Err(message) = &bundle {
            return Err(
                PluginManifestError::new(format!("spec.web.pages[{index}]"), message).into(),
            );
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
        Err(message) => {
            return Some(format!(
                "{message}; reinstall plugin '{namespace}' with `orbit plugin add --force`"
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

fn load_page_bundle(root: &Path, page: &PluginWebPage) -> Result<PluginPageBundle, String> {
    let id = &page.id;
    let declared_root = page.root.trim();
    let page_root = root.join(declared_root);
    match std::fs::symlink_metadata(&page_root) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(format!(
                "page '{id}': root '{declared_root}' is a symbolic link; a page root must be a \
                 directory inside the plugin root"
            ));
        }
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => {
            return Err(format!(
                "page '{id}': root '{declared_root}' is not a directory"
            ));
        }
        Err(error) => {
            return Err(format!(
                "page '{id}': root '{declared_root}' cannot be read: {error}"
            ));
        }
    }
    let page_root = std::fs::canonicalize(&page_root)
        .map_err(|error| format!("page '{id}': root '{declared_root}' cannot be read: {error}"))?;
    if !page_root.starts_with(root) {
        return Err(format!(
            "page '{id}': root '{declared_root}' resolves to {} outside the plugin root {}",
            page_root.display(),
            root.display()
        ));
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
        return Err(format!(
            "page '{id}': {PAGE_INDEX_FILE} holds a `{element}` element; Orbit writes the \
             page's scripts and styles itself, so list them under `scripts` and `styles`"
        ));
    }
    let mut scripts = Vec::with_capacity(page.scripts.len());
    for name in &page.scripts {
        let name = name.trim();
        let bytes = read_page_file(root, &page_root, page, name, &mut seen, &mut total)?;
        if let Some(found) = find_any_ascii_ci(&bytes, &["</script", "<!--"]) {
            return Err(format!(
                "page '{id}': script '{name}' contains `{found}`, which would end its `script` \
                 element early"
            ));
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
            return Err(format!(
                "page '{id}': style '{name}' contains `{found}`, which would end its `style` \
                 element early"
            ));
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
) -> Result<Vec<u8>, String> {
    let id = &page.id;
    let declared_root = page.root.trim();
    let path = page_root.join(name);
    let metadata = std::fs::symlink_metadata(&path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => {
            format!("page '{id}': '{name}' is missing from '{declared_root}'")
        }
        _ => format!("page '{id}': '{name}' cannot be read: {error}"),
    })?;
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "page '{id}': '{name}' is a symbolic link; a page file must be a regular file"
        ));
    }
    if !metadata.is_file() {
        return Err(format!("page '{id}': '{name}' is not a regular file"));
    }
    let resolved = std::fs::canonicalize(&path)
        .map_err(|error| format!("page '{id}': '{name}' cannot be read: {error}"))?;
    if !resolved.starts_with(root) {
        return Err(format!(
            "page '{id}': '{name}' resolves to {} outside the plugin root {}",
            resolved.display(),
            root.display()
        ));
    }
    if !seen.insert(resolved) {
        return Err(format!("page '{id}' lists '{name}' more than once"));
    }
    *total = total.saturating_add(metadata.len());
    if *total > MAX_PLUGIN_PAGE_BYTES {
        return Err(page_size_refusal(id, *total));
    }
    let bytes = std::fs::read(&path)
        .map_err(|error| format!("page '{id}': '{name}' cannot be read: {error}"))?;
    // The length is counted again from the bytes read, so a file that grew
    // between the stat and the read cannot carry the page past the bound.
    *total = total
        .saturating_sub(metadata.len())
        .saturating_add(bytes.len() as u64);
    if *total > MAX_PLUGIN_PAGE_BYTES {
        return Err(page_size_refusal(id, *total));
    }
    Ok(bytes)
}

fn page_size_refusal(id: &str, total: u64) -> String {
    format!(
        "page '{id}' holds at least {total} bytes; a page may hold at most \
         {MAX_PLUGIN_PAGE_BYTES} bytes (2 MiB) across {PAGE_INDEX_FILE} and its listed files"
    )
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
