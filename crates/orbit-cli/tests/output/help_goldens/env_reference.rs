use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use orbit_common::env_vars::ENVIRONMENT_VARIABLES;

use super::UPDATE_HELP_GOLDENS_ENV;

const BEGIN: &str = "<!-- BEGIN GENERATED ENVIRONMENT REFERENCE -->\n";
const END: &str = "<!-- END GENERATED ENVIRONMENT REFERENCE -->";

#[test]
fn config_environment_reference_matches_registry() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/config/environment.md");
    let contents = fs::read_to_string(&path).expect("read docs/config/environment.md");
    let (before, remainder) = contents
        .split_once(BEGIN)
        .expect("start marker for the generated environment reference");
    let (saved, after) = remainder
        .split_once(END)
        .expect("end marker for the generated environment reference");

    let mut table = String::from(
        "| Variable | Meaning | Default when unset | Flag relationship |\n\
         |---|---|---|---|\n",
    );
    let mut variables: Vec<_> = ENVIRONMENT_VARIABLES.iter().collect();
    variables.sort_unstable_by_key(|entry| entry.name);
    for entry in variables {
        writeln!(
            table,
            "| `{}` | {} | {} | {} |",
            entry.name,
            entry.meaning,
            entry.default,
            entry.flag.unwrap_or("—"),
        )
        .expect("write an environment reference row");
    }

    assert!(
        table.contains(
            "| `CARGO_DENY_DB_PATH` | Primary advisory database location for `scripts/cargo-deny.sh`; a non-empty value takes precedence over `ORBIT_CARGO_DENY_DB_PATH`. Orbit sets it for a sandboxed worker that may write. | If unset or empty, the wrapper uses `ORBIT_CARGO_DENY_DB_PATH`; if both are unset or empty, cargo-deny uses its own default location. | — |"
        ),
        "the generated environment reference must document cargo-deny's database path precedence and default"
    );

    if std::env::var(UPDATE_HELP_GOLDENS_ENV).as_deref() == Ok("1") && saved != table {
        fs::write(&path, format!("{before}{BEGIN}{table}{END}{after}"))
            .expect("update the generated environment reference in docs/config/environment.md");
    } else {
        assert!(
            saved == table,
            "docs/config/environment.md environment reference drifted from the registry. \
             Regenerate with make goldens UPDATE=1."
        );
    }
}
