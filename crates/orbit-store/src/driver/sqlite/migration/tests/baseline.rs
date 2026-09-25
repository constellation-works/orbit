use super::super::*;

#[test]
fn apply_schema_creates_adrs_table_and_indexes() {
    let conn = Connection::open_in_memory().expect("open in-memory connection");

    apply_schema(&conn).expect("apply schema");

    assert!(table_exists(&conn, "adrs").expect("adrs table exists"));

    let primary_key_columns: Vec<String> = conn
        .prepare("PRAGMA table_info(adrs)")
        .expect("prepare pragma")
        .query_map([], |row| {
            let name: String = row.get(1)?;
            let pk: i64 = row.get(5)?;
            Ok((name, pk))
        })
        .expect("query pragma")
        .filter_map(|row| {
            let (name, pk) = row.expect("pragma row");
            (pk > 0).then_some(name)
        })
        .collect();
    assert_eq!(primary_key_columns, vec!["id"]);
    assert!(table_has_column(&conn, "adrs", "tags").expect("tags column"));
    assert!(table_has_column(&conn, "adrs", "paths").expect("paths column"));

    for index_name in ["idx_adrs_status", "idx_adrs_owner"] {
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                     WHERE type = 'index' AND name = ?1",
                [index_name],
                |row| row.get(0),
            )
            .expect("query index");
        assert_eq!(count, 1, "expected index {index_name} to exist");
    }
}
