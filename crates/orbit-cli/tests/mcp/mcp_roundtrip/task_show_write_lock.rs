//! `orbit.task.show` through a fresh MCP server must not take the registry
//! write lock when `tasks.id_start` is already satisfied.
//!
//! A second connection holds `BEGIN IMMEDIATE` for the whole show. Before the
//! read-before-write floor check, that reserved lock made the show wait out
//! the busy timeout and return `store error: database is locked`.

use super::*;

#[test]
fn task_show_returns_the_task_while_the_registry_write_lock_is_held() {
    let test = concat!(
        module_path!(),
        "::task_show_returns_the_task_while_the_registry_write_lock_is_held"
    )
    .split_once("::")
    .expect("test module")
    .1;
    if crate::isolated_test::run_in_child(test) {
        return;
    }

    let workspace = McpWorkspace::init();
    let task_id = author_task(&workspace, "Show under a held registry write lock");
    let config_path = workspace.home.join(".orbit/config.toml");
    let raw = std::fs::read_to_string(&config_path).expect("read the fixture config");
    let mut document: toml_edit::DocumentMut = raw.parse().expect("parse the fixture config");
    document["tasks"]["id_start"] = toml_edit::value(1_i64);
    std::fs::write(&config_path, document.to_string()).expect("set tasks.id_start");

    let registry = workspace.home.join(".orbit/tasks/index.sqlite");
    let holder = Connection::open(&registry).expect("open the task registry");
    let next: i64 = holder
        .query_row(
            "SELECT next_number FROM allocator_state WHERE authority = 'local'",
            [],
            |row| row.get(0),
        )
        .expect("read the allocator counter");
    assert!(
        next >= 1,
        "the authored task must leave the allocator at or past id_start, got {next}"
    );
    holder
        .execute_batch("BEGIN IMMEDIATE")
        .expect("hold the registry write lock");

    let mut client = workspace.serve();
    let shown = client.show_task(&task_id);
    assert_eq!(shown["id"], task_id, "{shown}");
}
