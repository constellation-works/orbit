//! The `[machine]` table: global-only, read-only except for `machine.name`,
//! and admitted as one identity rather than three independent settings.

use super::{roots, write_config};

use crate::ResolvedConfig;
use tempfile::tempdir;

const IDENTITY: &str =
    "[machine]\nid = \"hm_0123456789abcdef\"\nname = \"dk-server-1\"\ntask_prefix = \"DE\"\n";

#[test]
fn a_workspace_machine_table_is_refused_at_load_naming_the_file() {
    let global = tempdir().expect("global");
    let workspace = tempdir().expect("workspace");
    write_config(global.path(), IDENTITY);
    write_config(
        workspace.path(),
        "[machine]\nid = \"hm_forged\"\nname = \"forged\"\ntask_prefix = \"FG\"\n",
    );

    let error = ResolvedConfig::load(&roots(global.path(), workspace.path()))
        .expect_err("a checkout must not be able to re-identify its machine")
        .to_string();
    assert!(
        error.contains("[machine] is not a workspace setting"),
        "{error}"
    );
    assert!(error.contains("config.toml"), "{error}");
    assert!(error.contains("--global"), "{error}");
}

#[test]
fn host_shutdown_hold_minutes_defaults_and_rejects_non_positive_values() {
    let global = tempdir().expect("global");
    let workspace = tempdir().expect("workspace");
    write_config(global.path(), "");
    let loaded = ResolvedConfig::load(&roots(global.path(), workspace.path())).expect("defaults");
    assert_eq!(loaded.snapshot.host_shutdown_hold_minutes(), 180);

    for minutes in [30_u32, 10_080] {
        write_config(
            global.path(),
            &format!("[machine]\nhost_shutdown_hold_minutes = {minutes}\n"),
        );
        let loaded = ResolvedConfig::load(&roots(global.path(), workspace.path()))
            .expect("a positive horizon loads");
        assert_eq!(loaded.snapshot.host_shutdown_hold_minutes(), minutes);
    }

    write_config(global.path(), "[machine]\nhost_shutdown_hold_minutes = 0\n");
    let zero = ResolvedConfig::load(&roots(global.path(), workspace.path()))
        .expect_err("zero is not a horizon")
        .to_string();
    assert!(
        zero.contains("machine.host_shutdown_hold_minutes") && zero.contains('0'),
        "{zero}"
    );

    write_config(
        global.path(),
        "[machine]\nhost_shutdown_hold_minutes = -1\n",
    );
    let negative = ResolvedConfig::load(&roots(global.path(), workspace.path()))
        .expect_err("a negative horizon is not an integer setting")
        .to_string();
    assert!(
        negative.contains("machine.host_shutdown_hold_minutes"),
        "{negative}"
    );
}
