use std::cell::Cell;
use std::fs::File;
use std::path::Path;
use std::time::{Duration, SystemTime};

use super::{Cache, ImageKey, MAX_ENTRIES, digest_with_cache, key_of, load, store};
use crate::OrbitError;

const CACHED: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const HASHED: &str = "2222222222222222222222222222222222222222222222222222222222222222";

/// Run the cache with a `hash` that counts how often it had to read the image.
fn digest(cache: &Path, image: &Path, hashes: &Cell<u32>) -> String {
    digest_with_cache(Some(cache), File::open(image).expect("image"), |_| {
        hashes.set(hashes.get() + 1);
        Ok::<_, OrbitError>(HASHED.to_string())
    })
    .expect("digest")
}

fn settle(cache: &Path, image: &Path, sha256: &str) {
    let key = key_of(&File::open(image).expect("image")).expect("unix file identity");
    let later = SystemTime::now() + Duration::from_secs(60);
    store(cache, key, sha256, later);
}

#[test]
fn a_freshly_written_image_is_hashed_every_time_and_never_cached() {
    let dir = tempfile::tempdir().expect("dir");
    let (cache, image) = (dir.path().join("cache.json"), dir.path().join("orbit"));
    std::fs::write(&image, b"fresh bytes").expect("image");
    let hashes = Cell::new(0);

    assert_eq!(digest(&cache, &image, &hashes), HASHED);
    assert_eq!(digest(&cache, &image, &hashes), HASHED);

    assert_eq!(hashes.get(), 2, "an unsettled image must not be trusted");
    assert!(!cache.exists(), "no entry may be written for a racy image");
}

#[test]
fn a_settled_image_is_answered_from_the_cache_without_reading_it() {
    let dir = tempfile::tempdir().expect("dir");
    let (cache, image) = (dir.path().join("cache.json"), dir.path().join("orbit"));
    std::fs::write(&image, b"installed bytes").expect("image");
    settle(&cache, &image, CACHED);
    let hashes = Cell::new(0);

    assert_eq!(digest(&cache, &image, &hashes), CACHED);

    assert_eq!(hashes.get(), 0);
}

#[test]
fn rewriting_the_image_invalidates_its_entry() {
    let dir = tempfile::tempdir().expect("dir");
    let (cache, image) = (dir.path().join("cache.json"), dir.path().join("orbit"));
    std::fs::write(&image, b"installed bytes").expect("image");
    settle(&cache, &image, CACHED);
    std::fs::write(&image, b"a different, longer image").expect("rewrite");
    let hashes = Cell::new(0);

    assert_eq!(digest(&cache, &image, &hashes), HASHED);

    assert_eq!(hashes.get(), 1);
}

#[test]
fn replacing_the_image_through_a_new_file_invalidates_its_entry() {
    let dir = tempfile::tempdir().expect("dir");
    let (cache, image) = (dir.path().join("cache.json"), dir.path().join("orbit"));
    std::fs::write(&image, b"installed bytes").expect("image");
    settle(&cache, &image, CACHED);
    let staged = dir.path().join("orbit.new");
    std::fs::write(&staged, b"installed bytes").expect("same-size candidate");
    std::fs::rename(&staged, &image).expect("swap");
    let hashes = Cell::new(0);

    assert_eq!(digest(&cache, &image, &hashes), HASHED);

    assert_eq!(hashes.get(), 1, "same size and content, different inode");
}

#[test]
fn an_unreadable_or_malformed_cache_falls_back_to_hashing() {
    let dir = tempfile::tempdir().expect("dir");
    let (cache, image) = (dir.path().join("cache.json"), dir.path().join("orbit"));
    std::fs::write(&image, b"installed bytes").expect("image");
    let hashes = Cell::new(0);

    std::fs::write(&cache, b"{not json").expect("corrupt cache");
    assert_eq!(digest(&cache, &image, &hashes), HASHED);

    settle(&cache, &image, "not-a-digest");
    assert_eq!(digest(&cache, &image, &hashes), HASHED);

    assert_eq!(hashes.get(), 2);
}

#[test]
fn without_a_cache_location_the_image_is_hashed() {
    let dir = tempfile::tempdir().expect("dir");
    let image = dir.path().join("orbit");
    std::fs::write(&image, b"installed bytes").expect("image");

    let digest = digest_with_cache(None, File::open(&image).expect("image"), |_| {
        Ok::<_, OrbitError>(HASHED.to_string())
    })
    .expect("digest");

    assert_eq!(digest, HASHED);
}

#[test]
fn a_missing_root_is_not_created_to_hold_the_cache() {
    let dir = tempfile::tempdir().expect("dir");
    let cache = dir.path().join("missing-root").join("cache.json");
    let image = dir.path().join("orbit");
    std::fs::write(&image, b"installed bytes").expect("image");

    settle(&cache, &image, CACHED);

    assert!(!cache.parent().expect("parent").exists());
}

#[test]
fn the_cache_remembers_a_bounded_number_of_distinct_images() {
    let dir = tempfile::tempdir().expect("dir");
    let cache = dir.path().join("cache.json");
    let later = SystemTime::now() + Duration::from_secs(60);
    for ino in 0..(MAX_ENTRIES as u64 + 4) {
        let key = ImageKey {
            dev: 1,
            ino,
            size: 10,
            mtime_sec: 1,
            mtime_nsec: 0,
            ctime_sec: 1,
            ctime_nsec: 0,
        };
        store(&cache, key, CACHED, later);
    }

    let Cache { entries, .. } = load(&cache).expect("cache");

    assert_eq!(entries.len(), MAX_ENTRIES);
    assert_eq!(
        entries[0].key.ino,
        MAX_ENTRIES as u64 + 3,
        "the newest image is first"
    );
}
