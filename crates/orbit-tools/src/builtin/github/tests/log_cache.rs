use std::fs::{self, File};
use std::time::{Duration, SystemTime};

use tempfile::TempDir;

use super::super::log_cache::{LogCache, LogCacheKey, RedactedLog};

fn redacted(source: &[u8]) -> RedactedLog {
    RedactedLog::from_source(source).unwrap()
}

fn key(name: &str) -> LogCacheKey {
    LogCacheKey::job(
        "https://api.github.com/repos/acme/orbit",
        "1",
        name,
        "failed",
    )
}

fn age(cache_dir: &std::path::Path, by: Duration) {
    for entry in fs::read_dir(cache_dir).unwrap().flatten() {
        let file = File::options().write(true).open(entry.path()).unwrap();
        file.set_modified(SystemTime::now() - by).unwrap();
    }
}

#[test]
fn an_entry_past_the_age_bound_is_removed_and_missed() {
    let dir = TempDir::new().unwrap();
    let cache = LogCache::new(dir.path().join("c"), 1 << 20, Duration::from_secs(60));
    cache.put(&key("1"), &redacted(b"log one"));
    assert_eq!(cache.get(&key("1")).as_deref(), Some(&b"log one"[..]));

    age(&dir.path().join("c"), Duration::from_secs(120));
    assert_eq!(cache.get(&key("1")), None, "expired entry must be a miss");
    assert_eq!(fs::read_dir(dir.path().join("c")).unwrap().count(), 0);
}

#[test]
fn a_store_removes_expired_entries_and_stray_staging_files() {
    let dir = TempDir::new().unwrap();
    let cache_dir = dir.path().join("c");
    let cache = LogCache::new(cache_dir.clone(), 1 << 20, Duration::from_secs(60));
    cache.put(&key("old"), &redacted(b"old"));
    fs::write(cache_dir.join(".staging-crashed"), b"partial").unwrap();
    age(&cache_dir, Duration::from_secs(120));

    cache.put(&key("new"), &redacted(b"new"));
    let names: Vec<_> = fs::read_dir(&cache_dir)
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name())
        .collect();
    assert_eq!(names.len(), 1, "only the fresh entry remains: {names:?}");
    assert_eq!(cache.get(&key("new")).as_deref(), Some(&b"new"[..]));
}

#[test]
fn the_size_bound_removes_the_oldest_entries_first() {
    let dir = TempDir::new().unwrap();
    let cache_dir = dir.path().join("c");
    let cache = LogCache::new(cache_dir.clone(), 25, Duration::from_secs(3600));
    cache.put(&key("a"), &redacted(&[b'a'; 10]));
    age(&cache_dir, Duration::from_secs(30));
    cache.put(&key("b"), &redacted(&[b'b'; 10]));
    age(&cache_dir, Duration::from_secs(20));
    // Refresh `a` and `b` ages relative to each other: a is older than b.
    File::options()
        .write(true)
        .open(cache_dir.join(format!("{}.log", digest_name(&cache_dir, b'a'))))
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(40))
        .unwrap();
    cache.put(&key("c"), &redacted(&[b'c'; 10]));

    assert_eq!(cache.get(&key("a")), None, "the oldest entry is evicted");
    assert!(cache.get(&key("b")).is_some());
    assert!(cache.get(&key("c")).is_some());

    cache.put(&key("huge"), &redacted(&[b'x'; 26]));
    assert_eq!(
        cache.get(&key("huge")),
        None,
        "a log over the bound is not stored"
    );
}

/// The entry file whose content is `fill` repeated, found without naming keys.
fn digest_name(dir: &std::path::Path, fill: u8) -> String {
    for entry in fs::read_dir(dir).unwrap().flatten() {
        if fs::read(entry.path()).unwrap().first() == Some(&fill) {
            return entry
                .path()
                .file_stem()
                .unwrap()
                .to_string_lossy()
                .into_owned();
        }
    }
    panic!("no entry holds {fill}");
}

#[cfg(unix)]
#[test]
fn entries_are_owner_only_and_a_staging_file_is_never_an_entry() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new().unwrap();
    let cache_dir = dir.path().join("c");
    let cache = LogCache::new(cache_dir.clone(), 1 << 20, Duration::from_secs(3600));
    cache.put(&key("1"), &redacted(b"secret-bearing log"));

    let mode = |path: &std::path::Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&cache_dir), 0o700);
    let entries: Vec<_> = fs::read_dir(&cache_dir).unwrap().flatten().collect();
    assert_eq!(entries.len(), 1, "no staging file survives a write");
    assert_eq!(mode(&entries[0].path()), 0o600);

    // A half-written download sits in a staging file, never under an entry name.
    fs::write(cache_dir.join(".staging-half"), b"partial").unwrap();
    assert_eq!(cache.get(&key("2")), None);
}

/// A reader or prune that classified an expired entry must not remove the
/// fresh entry a concurrent store published under the same name before the
/// removal ran; an unchanged expired entry is still removed.
#[test]
fn a_fresh_publication_survives_the_removal_of_the_entry_it_replaced() {
    let dir = TempDir::new().unwrap();
    let cache_dir = dir.path().join("c");
    let cache = LogCache::new(cache_dir.clone(), 1 << 20, Duration::from_secs(60));
    cache.put(&key("1"), &redacted(b"old log"));
    age(&cache_dir, Duration::from_secs(120));
    let path = fs::read_dir(&cache_dir)
        .unwrap()
        .flatten()
        .next()
        .unwrap()
        .path();
    let stale = fs::symlink_metadata(&path).unwrap();

    cache.put(&key("1"), &redacted(b"fresh log"));
    assert!(!cache.remove_if_unchanged(&path, &stale));
    assert!(cache.get(&key("1")).is_some());

    // An unchanged expired entry is still removed.
    age(&cache_dir, Duration::from_secs(120));
    let expired = fs::symlink_metadata(&path).unwrap();
    assert!(cache.remove_if_unchanged(&path, &expired));
    assert_eq!(cache.get(&key("1")), None);
}
