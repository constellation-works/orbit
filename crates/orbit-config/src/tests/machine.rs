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
