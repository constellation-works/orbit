#![allow(missing_docs)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use sha2::{Digest, Sha256};
use tempfile::tempdir;

use super::super::blob_store::BlobStore;
use crate::security::redaction::{PatternRedactor, redact_all};
use crate::test_env::scoped;

#[test]
fn write_hashes_and_stores_env_redacted_bytes() {
    let temp = tempdir().expect("tempdir");
    let secret = "live-audit-blob-secret-value";
    let _guard = scoped([("ORBIT_BLOB_STORE_TEST_TOKEN", Some(secret))]);
    let store = BlobStore::new(temp.path());
    let raw = format!("stdout contains {secret}\nAuthorization: Bearer pattern-secret-token\n");

    let hash = store.write(raw.as_bytes()).expect("write blob");
    let stored = store.read(&hash).expect("read blob");
    let stored_text = String::from_utf8(stored).expect("stored utf8");

    assert!(!stored_text.contains(secret));
    assert!(!stored_text.contains("pattern-secret-token"));
    assert!(stored_text.contains("[REDACTED_ENV]"));
    assert!(stored_text.contains("[REDACTED_AUTH]"));
    assert_eq!(hash, sha256_hex(redact_all(&raw).as_bytes()));
}

#[test]
fn caller_redaction_cannot_weaken_default_redaction() {
    let temp = tempdir().expect("tempdir");
    let secret = "live-audit-blob-empty-redactor-secret";
    let _guard = scoped([("ORBIT_BLOB_EMPTY_REDACTOR_TOKEN", Some(secret))]);
    let store = BlobStore::new(temp.path()).with_redaction(PatternRedactor::empty());
    let raw = format!("{secret}\n{{\"api_key\":\"json-secret\"}}\n");

    let hash = store.write(raw.as_bytes()).expect("write blob");
    let stored = store.read(&hash).expect("read blob");
    let stored_text = String::from_utf8(stored).expect("stored utf8");

    assert!(!stored_text.contains(secret));
    assert!(!stored_text.contains("json-secret"));
    assert!(stored_text.contains("[REDACTED_ENV]"));
    assert!(stored_text.contains("[REDACTED_AUTH]"));
    assert_eq!(hash, sha256_hex(redact_all(&raw).as_bytes()));
}

#[test]
fn write_redacts_secret_patterns_in_non_utf8_blob() {
    let temp = tempdir().expect("tempdir");
    let store = BlobStore::new(temp.path());
    let secret = b"nonutf-secret-token";
    let mut raw = b"stdout prefix\nAuthorization: Bearer ".to_vec();
    raw.extend_from_slice(secret);
    raw.extend_from_slice(b"\ninvalid byte follows: ");
    raw.push(0xff);

    let hash = store.write(&raw).expect("write blob");
    let stored = store.read(&hash).expect("read blob");
    let stored_text = String::from_utf8(stored.clone()).expect("stored lossy utf8");
    let expected = redact_all(&String::from_utf8_lossy(&raw));

    assert!(!stored.windows(secret.len()).any(|window| window == secret));
    assert!(!stored_text.contains("nonutf-secret-token"));
    assert!(stored_text.contains("Authorization: [REDACTED_AUTH]"));
    assert_eq!(hash, sha256_hex(expected.as_bytes()));
}

#[test]
fn caller_redaction_can_add_stronger_patterns() {
    let temp = tempdir().expect("tempdir");
    let store = BlobStore::new(temp.path()).with_redaction(PatternRedactor::with_argv_secrets());
    let raw = "argv accidentally contains sk-short\n";

    let hash = store.write(raw.as_bytes()).expect("write blob");
    let stored = store.read(&hash).expect("read blob");
    let stored_text = String::from_utf8(stored).expect("stored utf8");
    let expected = PatternRedactor::with_argv_secrets().apply_str(&redact_all(raw));

    assert!(!stored_text.contains("sk-short"));
    assert!(stored_text.contains("[REDACTED_API_KEY]"));
    assert_eq!(hash, sha256_hex(expected.as_bytes()));
}

#[cfg(unix)]
#[test]
fn write_creates_private_blob_file_and_dirs() {
    let temp = tempdir().expect("tempdir");
    let root = temp.path().join("audit").join("blobs");
    let store = BlobStore::new(&root);

    let hash = store.write(b"audit payload").expect("write blob");
    let shard_dir = root.join(&hash[..2]);
    let blob_path = shard_dir.join(&hash);

    assert_eq!(mode(&blob_path), 0o600);
    assert_eq!(mode(&root), 0o700);
    assert_eq!(mode(&shard_dir), 0o700);
}

#[test]
fn read_prefix_returns_at_most_max_bytes_without_loading_the_rest() {
    let temp = tempdir().expect("tempdir");
    let store = BlobStore::new(temp.path());
    let content = vec![b'x'; 64 * 1024];
    let hash = store.write(&content).expect("write blob");

    let prefix = store.read_prefix(&hash, 32).expect("read prefix");
    assert_eq!(prefix, content[..32]);
    assert_eq!(
        store.read(&hash).expect("read full blob").len(),
        content.len()
    );
}

#[test]
fn read_prefix_returns_the_whole_blob_when_shorter_than_max() {
    let temp = tempdir().expect("tempdir");
    let store = BlobStore::new(temp.path());
    let hash = store.write(b"short\n").expect("write blob");

    let prefix = store.read_prefix(&hash, 4096).expect("read prefix");
    assert_eq!(prefix, b"short\n");
}

#[test]
fn reads_refuse_references_that_are_not_sha256_digests() {
    let dir = tempdir().expect("tempdir");
    let store = BlobStore::new(dir.path().join("blobs"));
    let hash = store.write(b"payload").expect("write blob");
    assert_eq!(store.read(&hash).expect("read blob"), b"payload");

    for reference in [
        "",
        "a",
        "a\u{e9}",
        "../../etc/passwd",
        "error: write failed",
        &hash.to_uppercase(),
    ] {
        for error in [
            store.read(reference).expect_err("read must refuse"),
            store
                .read_prefix(reference, 4)
                .expect_err("read_prefix must refuse"),
        ] {
            assert_eq!(
                error.kind(),
                std::io::ErrorKind::InvalidInput,
                "{reference:?}"
            );
        }
    }
}

#[test]
fn write_repairs_a_corrupt_existing_blob() {
    let temp = tempdir().expect("tempdir");
    let store = BlobStore::new(temp.path());
    let content = b"complete audit payload";
    let hash = sha256_hex(content);
    let path = temp.path().join(&hash[..2]).join(&hash);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("create shard");
    std::fs::write(&path, b"truncated").expect("seed corrupt blob");

    assert_eq!(store.write(content).expect("repair blob"), hash);
    let repaired = std::fs::read(path).expect("read repaired blob");
    assert_eq!(sha256_hex(&repaired), hash);
    assert_eq!(repaired, content);

    #[cfg(unix)]
    assert_eq!(mode(&temp.path().join(&hash[..2]).join(&hash)), 0o600);
}

#[test]
fn concurrent_writes_never_publish_partial_blob_content() {
    let temp = tempdir().expect("tempdir");
    let store = Arc::new(BlobStore::new(temp.path()));
    let content = Arc::new(vec![b'x'; 4 * 1024 * 1024]);
    let hash = sha256_hex(&content);
    let path = temp.path().join(&hash[..2]).join(&hash);
    let active_workers = AtomicUsize::new(0);

    run_concurrent_writes(
        Arc::clone(&store),
        Arc::clone(&content),
        path.clone(),
        &active_workers,
        |_, store, content| {
            assert_eq!(
                store.write(content).expect("write blob"),
                sha256_hex(content)
            );
        },
    );

    assert_eq!(std::fs::read(path).expect("final blob"), *content);
    assert_eq!(active_workers.load(Ordering::Acquire), 0);
}

#[test]
fn concurrent_writer_panic_stops_workers_and_preserves_cause() {
    let temp = tempdir().expect("tempdir");
    let store = Arc::new(BlobStore::new(temp.path()));
    let content = Arc::new(vec![b'x'; 4 * 1024 * 1024]);
    let hash = sha256_hex(&content);
    let path = temp.path().join(&hash[..2]).join(&hash);
    let active_workers = AtomicUsize::new(0);

    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_concurrent_writes(
            Arc::clone(&store),
            Arc::clone(&content),
            path,
            &active_workers,
            |writer_index, store, content| {
                if writer_index == 0 {
                    panic!("injected writer failure");
                }
                store.write(content).expect("write blob");
            },
        );
    }));

    let panic = outcome.expect_err("injected writer panic should fail the fixture");
    let panic_message = panic
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
        .expect("writer panic should retain its message");
    assert_eq!(panic_message, "injected writer failure");
    assert_eq!(
        active_workers.load(Ordering::Acquire),
        0,
        "all reader and writer workers should stop before the panic is reported"
    );
}

fn run_concurrent_writes<F>(
    store: Arc<BlobStore>,
    content: Arc<Vec<u8>>,
    path: std::path::PathBuf,
    active_workers: &AtomicUsize,
    write: F,
) where
    F: Fn(usize, &BlobStore, &[u8]) + Sync,
{
    let complete = Arc::new(AtomicBool::new(false));
    let remaining_writers = Arc::new(AtomicUsize::new(2));

    std::thread::scope(|scope| {
        let mut writer_handles = Vec::with_capacity(2);
        for writer_index in 0..2 {
            let store = Arc::clone(&store);
            let content = Arc::clone(&content);
            let complete = Arc::clone(&complete);
            let remaining_writers = Arc::clone(&remaining_writers);
            let write = &write;
            writer_handles.push(scope.spawn(move || {
                let _active = ActiveWorker::new(active_workers);
                let _completion = WriterCompletion {
                    remaining_writers,
                    complete,
                };
                write(writer_index, &store, &content);
            }));
        }

        let complete = Arc::clone(&complete);
        let reader_content = Arc::clone(&content);
        let reader_path = path;
        let reader_handle = scope.spawn(move || {
            let _active = ActiveWorker::new(active_workers);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            while !complete.load(Ordering::Acquire) {
                assert!(
                    std::time::Instant::now() < deadline,
                    "blob reader must stop when all writers exit"
                );
                match std::fs::read(&reader_path) {
                    Ok(observed) => {
                        assert_eq!(observed, *reader_content, "partial final blob was visible")
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => panic!("read blob: {error}"),
                }
                std::thread::yield_now();
            }
        });

        let mut first_panic = None;
        for handle in writer_handles {
            if let Err(panic) = handle.join()
                && first_panic.is_none()
            {
                first_panic = Some(panic);
            }
        }
        if let Err(panic) = reader_handle.join()
            && first_panic.is_none()
        {
            first_panic = Some(panic);
        }
        if let Some(panic) = first_panic {
            std::panic::resume_unwind(panic);
        }
    });
}

struct ActiveWorker<'a>(&'a AtomicUsize);

impl<'a> ActiveWorker<'a> {
    fn new(active_workers: &'a AtomicUsize) -> Self {
        active_workers.fetch_add(1, Ordering::AcqRel);
        Self(active_workers)
    }
}

impl Drop for ActiveWorker<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

struct WriterCompletion {
    remaining_writers: Arc<AtomicUsize>,
    complete: Arc<AtomicBool>,
}

impl Drop for WriterCompletion {
    fn drop(&mut self) {
        if self.remaining_writers.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.complete.store(true, Ordering::Release);
        }
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{:02x}", byte));
    }
    out
}

#[cfg(unix)]
fn mode(path: &std::path::Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;

    std::fs::metadata(path)
        .expect("metadata")
        .permissions()
        .mode()
        & 0o777
}
