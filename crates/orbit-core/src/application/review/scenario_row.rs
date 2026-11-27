//! [ORB-29076] Consolidated scenario rows: the rows a `scenario_table!` runs
//! inside one libtest. A row's Bubblewrap deferral names the row, not the
//! libtest, so a Cargo filter on the row runs no test. The runnable native
//! check is the enclosing libtest, and it validates the row only when it
//! prints the row's `qa-scenario-pass:` marker.
//!
//! The index is read from the candidate's Git tree, like [`CargoOwners`], and
//! a row yields a home only when exactly one libtest in the workspace runs it.

use std::collections::{BTreeMap, BTreeSet};

use git2::{ObjectType, Repository, Tree};
use orbit_types::workflow::{ReviewValidation, ValidationOutcome, ValidationRole};

use super::cargo_target::CargoOwners;
use super::evidence::deferred_test;

/// The libtest a scenario row runs in: its package, integration target and
/// the test path `cargo test` filters on (`module::table`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct ScenarioHome {
    package: String,
    target: String,
    test: String,
}

/// Scenario rows keyed by row name. A row can be indexed without a home when
/// several libtests run it, which makes the row ambiguous.
#[derive(Default)]
pub(super) struct ScenarioRows {
    homes: BTreeMap<String, BTreeSet<ScenarioHome>>,
}

impl ScenarioRows {
    /// Whether `row` is a row some `scenario_table!` in the candidate runs.
    /// Such a deferral never names a runnable Cargo filter.
    pub(super) fn is_row(&self, row: &str) -> bool {
        self.homes.contains_key(row)
    }

    /// The one libtest that runs `row`, when the candidate proves exactly one.
    fn unique_home(&self, row: &str) -> Option<&ScenarioHome> {
        let homes = self.homes.get(row)?;
        homes.iter().next().filter(|_| homes.len() == 1)
    }

    /// Index every row of the `tests/<target>` directory `dir` of `package`.
    /// A target that uses `#[path]`, a module declared twice or an undeclared
    /// module contributes nothing, so no row there has a home.
    pub(super) fn scan_target(
        &mut self,
        repo: &Repository,
        dir: &Tree<'_>,
        package: &str,
        target: &str,
    ) {
        let mut files = BTreeMap::new();
        if collect_modules(repo, dir, "", &mut files).is_none() {
            return;
        }
        if files.values().any(|text| text.contains("#[path")) {
            return;
        }
        for (module, text) in &files {
            if module.is_empty() || !declared_chain(&files, module) {
                continue;
            }
            for (table, rows) in scenario_tables(text) {
                let home = ScenarioHome {
                    package: package.to_string(),
                    target: target.to_string(),
                    test: format!("{module}::{table}"),
                };
                for row in rows {
                    self.homes.entry(row).or_default().insert(home.clone());
                }
            }
        }
    }
}

/// Operator diagnostics for required records whose deferral notices name a
/// scenario row. A row with one home gets the native check for that libtest
/// and its marker; any other row names the uncovered command and the
/// fresh-review route. Neither offers the row as a Cargo filter.
pub(super) fn unreconciled_scenario_deferrals(
    records: &[ReviewValidation],
    owners: &CargoOwners,
) -> Vec<String> {
    let mut diagnostics = Vec::new();
    for record in records {
        if record.role != ValidationRole::Required
            || record.outcome != ValidationOutcome::Passed
            || record.deferred.is_empty()
        {
            continue;
        }
        let rows = owners.scenarios();
        let label = match record.record_id() {
            Some(id) => format!("`{id}` (`{}`)", record.command),
            None => format!("`{}`", record.command),
        };
        for row in record
            .deferred
            .iter()
            .filter_map(|notice| deferred_test(notice))
            .filter(|row| rows.is_row(row))
        {
            diagnostics.push(match rows.unique_home(row) {
                Some(home) => format!(
                    "unreconciled_scenario_deferral: required record {label} defers scenario row \
                     `{row}`, which runs only inside `{}` of package `{}` (`--test {}`). Run \
                     `cargo test -p {} --test {} {}` natively outside the sandbox without \
                     deferral and confirm its output prints `qa-scenario-pass: {row}`. That \
                     validates the row alone: a parent test pass without that marker, or a \
                     repeated deferral, does not settle it, and the broader required record \
                     `{}` still needs its own admissible validation, so admit a fresh review \
                     with matching required and host_sandbox_test records",
                    home.test,
                    home.package,
                    home.target,
                    home.package,
                    home.target,
                    home.test,
                    record.command,
                ),
                None => format!(
                    "unreconciled_scenario_deferral: required record {label} defers scenario row \
                     `{row}`, which no single libtest in the candidate runs, so no native check \
                     is named for it. Run `{}` natively outside the sandbox without deferral and \
                     admit a fresh review with that result",
                    record.command,
                ),
            });
        }
    }
    diagnostics
}

/// Module path to text for every `.rs` file of a target directory. The root
/// (`main.rs`) is keyed by the empty path. `None` when two files claim one
/// module or the tree holds an unreadable entry.
fn collect_modules(
    repo: &Repository,
    dir: &Tree<'_>,
    module: &str,
    files: &mut BTreeMap<String, String>,
) -> Option<()> {
    for item in dir {
        let name = item.name().ok()?;
        match item.kind()? {
            ObjectType::Blob => {
                let Some(stem) = name.strip_suffix(".rs") else {
                    continue;
                };
                let key = match (stem, module.is_empty()) {
                    ("main", true) => String::new(),
                    ("mod", _) => module.to_string(),
                    _ => join(module, stem),
                };
                if key.is_empty() && stem != "main" {
                    return None;
                }
                let blob = repo.find_blob(item.id()).ok()?;
                let text = String::from_utf8(blob.content().to_vec()).ok()?;
                if files.insert(key, text).is_some() {
                    return None;
                }
            }
            ObjectType::Tree => {
                let sub = item.to_object(repo).ok()?.peel_to_tree().ok()?;
                collect_modules(repo, &sub, &join(module, name), files)?;
            }
            _ => return None,
        }
    }
    Some(())
}

fn join(module: &str, segment: &str) -> String {
    if module.is_empty() {
        segment.to_string()
    } else {
        format!("{module}::{segment}")
    }
}

/// Whether every segment of `module` is declared by a plain `mod` line in the
/// file of its parent module.
fn declared_chain(files: &BTreeMap<String, String>, module: &str) -> bool {
    let segments: Vec<&str> = module.split("::").collect();
    (0..segments.len()).all(|index| {
        let parent = segments[..index].join("::");
        files
            .get(&parent)
            .is_some_and(|text| declares(text, segments[index]))
    })
}

fn declares(text: &str, name: &str) -> bool {
    text.lines().any(|line| {
        let line = line.trim_start();
        let line = line
            .strip_prefix("pub(crate) ")
            .or_else(|| line.strip_prefix("pub "))
            .unwrap_or(line);
        line.strip_prefix("mod ")
            .and_then(|rest| rest.strip_prefix(name))
            .is_some_and(|rest| rest.trim_start().starts_with(';'))
    })
}

/// The `(table, rows)` of every `scenario_table!(table, row, …)` in `text`.
fn scenario_tables(text: &str) -> Vec<(String, Vec<String>)> {
    const OPEN: &str = "scenario_table!(";
    let mut tables = Vec::new();
    let mut rest = text;
    while let Some(index) = rest.find(OPEN) {
        let after = &rest[index + OPEN.len()..];
        let Some(end) = after.find(')') else {
            break;
        };
        let args: Vec<&str> = after[..end]
            .split(',')
            .map(str::trim)
            .filter(|arg| !arg.is_empty())
            .collect();
        if let Some((table, rows)) = args.split_first()
            && is_ident(table)
            && rows.iter().all(|row| is_ident(row))
        {
            tables.push((
                (*table).to_string(),
                rows.iter().map(|row| (*row).to_string()).collect(),
            ));
        }
        rest = &after[end..];
    }
    tables
}

fn is_ident(word: &str) -> bool {
    !word.is_empty()
        && !word.starts_with(|ch: char| ch.is_ascii_digit())
        && word
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}
