//! `execution.agent_stall_minutes` admits zero as "disabled".

use super::{roots, write_config};
use crate::ResolvedConfig;
use tempfile::tempdir;

#[test]
fn agent_stall_minutes_defaults_to_thirty_and_zero_disables() {
    let global = tempdir().expect("global");
    let workspace = tempdir().expect("workspace");
    write_config(global.path(), "");
    let loaded = ResolvedConfig::load(&roots(global.path(), workspace.path())).expect("defaults");
    assert_eq!(loaded.snapshot.execution_agent_stall_minutes, 30);

    write_config(global.path(), "[execution]\nagent_stall_minutes = 0\n");
    let disabled =
        ResolvedConfig::load(&roots(global.path(), workspace.path())).expect("zero disables");
    assert_eq!(disabled.snapshot.execution_agent_stall_minutes, 0);

    write_config(global.path(), "[execution]\nagent_stall_minutes = 15\n");
    let custom = ResolvedConfig::load(&roots(global.path(), workspace.path())).expect("custom");
    assert_eq!(custom.snapshot.execution_agent_stall_minutes, 15);

    write_config(global.path(), "[execution]\nagent_stall_minutes = 1441\n");
    let rejected = ResolvedConfig::load(&roots(global.path(), workspace.path()))
        .expect_err("above a day")
        .to_string();
    assert!(
        rejected.contains("execution.agent_stall_minutes") && rejected.contains("1441"),
        "{rejected}"
    );
}
