use super::super::*;

#[test]
fn run_archive_stage_migration_adds_archived_at() {
    let conn = Connection::open_in_memory().expect("open in-memory connection");
    apply_schema(&conn).expect("apply schema");
    assert!(table_has_column(&conn, "job_runs", "archived_at").expect("archived_at column"));
}
