//! Crew configuration shared by isolated CLI integration fixtures.

use std::fs;
use std::path::Path;

use toml_edit::{Array, DocumentMut, value};

/// Make the fixture's selected crew usable regardless of agent CLIs on PATH.
pub(crate) fn configure_sol(root: &Path) {
    let path = root.join("config.toml");
    let mut config: DocumentMut = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
        .parse()
        .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));

    config["crews"]["sol"]["enabled"] = value(true);
    config["crews"]["sol"]["provider"] = value("codex");
    config["crews"]["sol"]["model"] = value("gpt-6-sol");
    config["crews"]["sol"]["backend"] = value("cli");
    config["workflow"]["default_crew"] = value("sol");
    for pool in [
        "low_complexity_crews",
        "medium_complexity_crews",
        "hard_complexity_crews",
        "xhard_complexity_crews",
    ] {
        let mut crews = Array::new();
        crews.push("sol");
        config["workflow"][pool] = value(crews);
    }

    fs::write(&path, config.to_string())
        .unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
}
