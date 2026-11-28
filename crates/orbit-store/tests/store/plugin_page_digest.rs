//! Plugin page digests in the host store: a row written before the column
//! existed reads as a plugin without pages, and recorded digests round-trip.

use orbit_store::Store;

const PLUGIN_ROW: &str = "INSERT INTO plugins(name, version, source, install_path, \
     manifest_digest, enabled, grants_json) VALUES ('legacy', '0.1.0', '/src', '/install', \
     'abc', 1, '[]')";

#[test]
fn a_row_without_page_digests_reads_unchanged_and_digests_round_trip() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("orbit.db");
    drop(Store::open(&path).expect("create the store"));
    // The row an install before plugin pages left: the column stays NULL.
    let connection = rusqlite::Connection::open(&path).expect("open the database");
    connection
        .execute(PLUGIN_ROW, [])
        .expect("insert a legacy row");
    drop(connection);

    let store = Store::open(&path).expect("reopen the store");
    let legacy = store
        .read()
        .and_then(|snapshot| snapshot.plugin().get("legacy"))
        .expect("read")
        .expect("the row");
    assert!(legacy.page_digests.is_empty());
    assert_eq!(legacy.manifest_digest, "abc");
    assert!(legacy.enabled);

    let mut paged = legacy.clone();
    paged
        .page_digests
        .insert("overview".to_string(), "d1".to_string());
    store
        .write(|tx| tx.plugin().put(&paged))
        .expect("record digests");
    let read = store
        .read()
        .and_then(|snapshot| snapshot.plugin().get("legacy"))
        .expect("read")
        .expect("the row");
    assert_eq!(read.page_digests, paged.page_digests);

    // A reinstall without pages writes NULL again, as the legacy row had.
    store
        .write(|tx| tx.plugin().put(&legacy))
        .expect("record no digests");
    drop(store);
    let connection = rusqlite::Connection::open(&path).expect("open the database");
    let raw: Option<String> = connection
        .query_row(
            "SELECT page_digests_json FROM plugins WHERE name = 'legacy'",
            [],
            |row| row.get(0),
        )
        .expect("read the column");
    assert_eq!(raw, None);
}
