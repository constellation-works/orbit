use super::review_layers_write_config;
use orbit_config::ConfigRoots;

use orbit_config::ResolvedConfig;
use tempfile::tempdir;

const IDENTITY: &str =
    "[machine]\nid = \"hm_0123456789abcdef\"\nname = \"dk-server-1\"\ntask_prefix = \"DE\"\n";

#[test]
fn a_workspace_machine_table_is_refused_at_load_naming_the_file() {
    let global = tempdir().expect("global");
    let workspace = tempdir().expect("workspace");
    review_layers_write_config(global.path(), IDENTITY);
    review_layers_write_config(
        workspace.path(),
        "[machine]\nid = \"hm_forged\"\nname = \"forged\"\ntask_prefix = \"FG\"\n",
    );

    let error = ResolvedConfig::load(&ConfigRoots::new(global.path(), workspace.path()))
        .expect_err("a checkout must not be able to re-identify its machine")
        .to_string();
    assert!(
        error.contains("[machine] is not a workspace setting"),
        "{error}"
    );
    assert!(error.contains("config.toml"), "{error}");
    assert!(error.contains("--global"), "{error}");
}
