//! Evidence for the isolated-HOME guards in `routine_root` and `sweep_root`:
//! when a command writes into the fixture HOME, the guard names every entry
//! it found, with its kind, size and modification time, instead of only the
//! directory.

use std::fs;
use std::path::Path;
use std::time::UNIX_EPOCH;

/// Every path under `home`, relative to it, one line each.
pub(crate) fn isolated_home_entries(home: &Path) -> Vec<String> {
    let mut entries = Vec::new();
    collect(home, home, &mut entries);
    entries.sort();
    entries
}

fn collect(home: &Path, dir: &Path, entries: &mut Vec<String>) {
    let listing = fs::read_dir(dir).expect("read isolated home");
    for entry in listing {
        let entry = entry.expect("read isolated home entry");
        let path = entry.path();
        let relative = path.strip_prefix(home).unwrap_or(&path).display();
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            entries.push(format!("{relative} (vanished while listed)"));
            continue;
        };
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map_or_else(
                || "unknown".to_string(),
                |at| format!("{:.3}", at.as_secs_f64()),
            );
        let kind = if metadata.is_dir() {
            "dir"
        } else if metadata.is_symlink() {
            "symlink"
        } else {
            "file"
        };
        entries.push(format!(
            "{relative} ({kind}, {} bytes, modified at unix {modified})",
            metadata.len()
        ));
        if metadata.is_dir() {
            collect(home, &path, entries);
        }
    }
}
