//! The retained host-shutdown horizon admission [ORB-15796]. Public machine
//! layer refusal is exercised by the crate-root review_layers integration target.

use super::{roots, write_config};

use crate::ResolvedConfig;
use tempfile::tempdir;

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
