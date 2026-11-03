//  The audit blob sweep beside a live writer: each interleaving of a write,
//  its publication and the sweep's removal keeps every blob a row may name.

const GRACE: Duration = Duration::from_secs(24 * 60 * 60);
const MARKER_TTL: Duration = Duration::from_secs(60 * 24 * 60 * 60);

struct Fixture {
    _dir: tempfile::TempDir,
    audit_root: std::path::PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let audit_root = dir.path().join("audit");
        Self {
            _dir: dir,
            audit_root,
        }
    }

    /// The writer the run audit sink uses.
    fn writer(&self) -> BlobStore {
        BlobStore::new(self.audit_root.join("blobs"))
            .with_pending_root(blob_sweep::pending_root(&self.audit_root))
    }

    fn sweep(&self) -> AuditBlobRoot {
        AuditBlobRoot::new(&self.audit_root)
    }

    fn marker(&self, hash: &str) -> std::path::PathBuf {
        blob_sweep::pending_root(&self.audit_root).join(hash)
    }
}

fn age(path: &Path, by: Duration) {
    std_fs::OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .set_modified(SystemTime::now() - by)
        .unwrap();
}

/// The one listed blob, as the sweep's scan saw it.
fn listed(sweep: &AuditBlobRoot, hash: &str) -> SweepEntry {
    sweep
        .blobs()
        .unwrap()
        .into_iter()
        .find(|blob| blob.hash == hash)
        .expect("blob listed")
}

fn remove(sweep: &AuditBlobRoot, blob: &SweepEntry) -> SweepOutcome {
    let now = SystemTime::now();
    sweep
        .remove_unless_protected(blob, now - GRACE, now - MARKER_TTL)
        .unwrap()
}

#[test]
fn written_but_unpublished_blob_is_never_swept() {
    let fixture = Fixture::new();
    let writer = fixture.writer();
    let hash = writer.write(b"tool output not yet in any row").unwrap();
    assert!(fixture.marker(&hash).is_file(), "marked before publication");

    // Even past the grace window, the pending marker protects it.
    let sweep = fixture.sweep();
    let blob = listed(&sweep, &hash);
    age(&blob.path, 2 * GRACE);
    let blob = listed(&sweep, &hash);
    assert_eq!(remove(&sweep, &blob), SweepOutcome::Retained);
    assert_eq!(
        writer.read(&hash).unwrap(),
        b"tool output not yet in any row"
    );

    // Once its row is durable the marker retires, and an unreferenced old
    // blob is reclaimable.
    writer.clear_published(&format!("{{\"output_ref\":\"{hash}\"}}"));
    assert!(!fixture.marker(&hash).exists());
    assert_eq!(remove(&sweep, &blob), SweepOutcome::Removed);
    assert!(writer.read(&hash).is_err());
    assert!(sweep.blobs().unwrap().is_empty());
}

#[test]
fn reuse_after_the_scan_keeps_the_blob() {
    let fixture = Fixture::new();
    let writer = fixture.writer();
    let hash = writer.write(b"shared output").unwrap();
    writer.clear_published(&hash);
    age(
        &fixture.writer().root().join(&hash[..2]).join(&hash),
        2 * GRACE,
    );

    // The sweep's reference scan finds nothing naming the old blob...
    let sweep = fixture.sweep();
    let blob = listed(&sweep, &hash);

    // ...then a writer stores the same content, publishes and retires its
    // marker before the sweep reaches the blob.
    assert_eq!(writer.write(b"shared output").unwrap(), hash);
    writer.clear_published(&hash);
    assert!(!fixture.marker(&hash).exists());

    assert_eq!(remove(&sweep, &blob), SweepOutcome::Retained);
    assert_eq!(writer.read(&hash).unwrap(), b"shared output");
}

#[test]
fn write_after_removal_stores_the_blob_again() {
    let fixture = Fixture::new();
    let writer = fixture.writer();
    let hash = writer.write(b"rewritten").unwrap();
    writer.clear_published(&hash);
    let sweep = fixture.sweep();
    age(&listed(&sweep, &hash).path, 2 * GRACE);
    assert_eq!(
        remove(&sweep, &listed(&sweep, &hash)),
        SweepOutcome::Removed
    );

    assert_eq!(writer.write(b"rewritten").unwrap(), hash);
    assert_eq!(writer.read(&hash).unwrap(), b"rewritten");
    // The rewrite is fresh, so the next sweep keeps it.
    assert_eq!(
        remove(&sweep, &listed(&sweep, &hash)),
        SweepOutcome::Retained
    );
}

#[test]
fn stale_markers_of_abandoned_writes_are_reclaimed() {
    let fixture = Fixture::new();
    let writer = fixture.writer();
    let hash = writer.write(b"crashed before its row").unwrap();
    let sweep = fixture.sweep();
    age(&listed(&sweep, &hash).path, 2 * MARKER_TTL);
    age(&fixture.marker(&hash), 2 * MARKER_TTL);

    // A marker older than the stale cutoff no longer protects its blob.
    assert_eq!(
        remove(&sweep, &listed(&sweep, &hash)),
        SweepOutcome::Removed
    );
    let marker = sweep
        .pending()
        .unwrap()
        .into_iter()
        .find(|marker| marker.hash == hash)
        .unwrap();
    assert!(
        sweep
            .remove_stale_marker(&marker, SystemTime::now() - MARKER_TTL)
            .unwrap()
    );
    assert!(sweep.pending().unwrap().is_empty());

    // A fresh marker is left alone.
    let fresh = writer.write(b"in flight").unwrap();
    let marker = sweep
        .pending()
        .unwrap()
        .into_iter()
        .find(|marker| marker.hash == fresh)
        .unwrap();
    assert!(
        !sweep
            .remove_stale_marker(&marker, SystemTime::now() - MARKER_TTL)
            .unwrap()
    );
}

#[test]
fn interrupted_sweep_returns_quarantined_blobs() {
    let fixture = Fixture::new();
    let writer = fixture.writer();
    let hash = writer.write(b"moved aside").unwrap();
    let blob_path = writer.root().join(&hash[..2]).join(&hash);
    let quarantine = fixture.audit_root.join("sweep");
    std_fs::create_dir_all(&quarantine).unwrap();
    std_fs::rename(&blob_path, quarantine.join(&hash)).unwrap();

    let sweep = fixture.sweep();
    let _lock = sweep.try_lock().unwrap().expect("sweep lock");
    assert!(sweep.try_lock().unwrap().is_none(), "one sweep at a time");
    assert_eq!(sweep.recover_quarantine().unwrap(), 1);
    assert_eq!(writer.read(&hash).unwrap(), b"moved aside");
}

/// The reference scan's tokenizer skips through prose a window at a time
/// [ORB-21830]. A token it missed would let the sweep delete a blob a
/// remaining row still names, so it must find exactly what splitting on every
/// non-hex character finds: maximal runs of 64 hex digits, all lowercase.
#[test]
fn reference_tokenizer_finds_every_whole_64_digit_hash() {
    fn by_split(text: &str) -> Vec<String> {
        text.split(|ch: char| !ch.is_ascii_hexdigit())
            .filter(|token| blob_sweep::is_blob_hash(token))
            .map(str::to_owned)
            .collect()
    }
    fn found(text: &str) -> Vec<String> {
        let mut hashes = Vec::new();
        blob_sweep::for_each_blob_hash(text, |hash| hashes.push(hash.to_owned()));
        hashes
    }
    let a = "a".repeat(64);
    let b = "0123456789abcdef".repeat(4);
    let mut texts = vec![
        String::new(),
        a.clone(),
        format!("{a}{b}"),
        format!("{a} {b}"),
        format!("x{a}"),
        format!("{a}x"),
        format!("é{a}ü {b}→"),
        format!("{}{}", "f".repeat(63), " ".repeat(64)),
        "f".repeat(65),
        format!("{}B{}", "c".repeat(63), &a[1..]),
        format!("{}A {b}", &a[1..]),
        format!("{{\"output_ref\":\"{a}\",\"stdout_ref\":\"{b}\"}}"),
        format!("deadbeef {} cafe {a}", "1".repeat(40)),
    ];
    // Every split of a hash against prose and other digits.
    for cut in 0..=64 {
        texts.push(format!("{}x{a}", &b[..cut]));
        texts.push(format!("{}{} {b}", "9".repeat(cut), &a[..64 - cut]));
        texts.push(format!("the run {} passed {}", &a[..cut], &b[cut..]));
    }
    // Deterministic mixes of digit runs, case changes and multi-byte chars.
    let pieces = ["a", "f", "0", "9", "A", " ", "é", "x", &a[..32], &b[..60]];
    let mut seed = 21_830u64;
    for _ in 0..2_000 {
        let mut text = String::new();
        for _ in 0..40 {
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            text.push_str(pieces[(seed >> 33) as usize % pieces.len()]);
        }
        texts.push(text);
    }
    for text in &texts {
        assert_eq!(
            found(text),
            by_split(text),
            "tokenizer disagreed on {text:?}"
        );
    }
    assert_eq!(found(&format!("é{a}ü {b}→")), vec![a.clone(), b.clone()]);
}
