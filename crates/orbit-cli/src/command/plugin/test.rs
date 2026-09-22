use std::path::PathBuf;

use clap::Args;
use orbit_core::OrbitRuntime;
use orbit_core::adapter::command::PluginTestOptions;
use serde_json::json;

use crate::command::{Block, CommandOut, Execute, Payload};
use crate::output::color::Domain;

#[derive(Args)]
pub struct PluginTestArgs {
    /// Plugin directory holding `plugin.yaml`
    pub dir: PathBuf,
    /// Consent to these grants for this run (repeatable, comma-separated):
    /// fs, network, env_pass, orbit_tools, unsandboxed. Same names as
    /// `orbit plugin enable --grant`. Required when the manifest asks for an
    /// unconfined backend, an absolute write root, `network: any`, or
    /// `env_pass`, unless `--accept-requested` is set. Does not record a
    /// host grant.
    #[arg(long = "grant", value_delimiter = ',')]
    pub grants: Vec<String>,
    /// Run under the profile the manifest requests, including an unconfined
    /// backend, absolute write roots, `network: any`, and `env_pass`. Does
    /// not record a host grant.
    #[arg(long = "accept-requested")]
    pub accept_requested: bool,
}

impl Execute for PluginTestArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let report = runtime.test_plugin_dir(
            &self.dir,
            &PluginTestOptions {
                grants: self.grants,
                accept_requested: self.accept_requested,
            },
        )?;

        use crate::output::table::{Column, Table};
        use comfy_table::Cell;
        let mut table = Table::new(vec![
            Column::new("TEST").fixed(),
            Column::new("TOOL").fixed(),
            Column::new("RESULT").fixed(),
            Column::new("DETAIL"),
        ])
        .empty_message("this plugin ships no conformance goldens");
        for result in &report.results {
            table.add_row(vec![
                Cell::new(&result.name),
                Cell::new(&result.tool),
                crate::output::color::cell(
                    if result.passed { "passed" } else { "failed" },
                    Domain::JobState,
                ),
                Cell::new(&result.detail),
            ]);
        }

        let grant_line = format!("Requested grants: {}.", report.requested_grants);
        let summary = if report.passed() {
            format!(
                "{grant_line}\n{} of {} conformance test(s) passed against Orbit {}; {}",
                report.results.len(),
                report.results.len(),
                report.orbit_version,
                report.certification_note
            )
        } else {
            format!(
                "{grant_line}\n{} of {} conformance test(s) failed against Orbit {}: {}",
                report.failures().len(),
                report.results.len(),
                report.orbit_version,
                report.failures().join(", ")
            )
        };
        let doc = json!({
            "name": report.name,
            "version": report.version,
            "root": report.root,
            "manifest_digest": report.manifest_digest,
            "orbit_version": report.orbit_version,
            "requested_grants": report.requested_grants,
            "passed": report.passed(),
            "certified": report.certified,
            "certification": report.certification_note,
            "results": report
                .results
                .iter()
                .map(|result| json!({
                    "name": result.name,
                    "tool": result.tool,
                    "passed": result.passed,
                    "detail": result.detail,
                }))
                .collect::<Vec<_>>(),
        });
        // A failing suite exits non-zero: `orbit plugin test` is a gate a
        // plugin's own CI runs, and a gate that always exits 0 is not one.
        let exit_code = i32::from(!report.passed());
        Ok(
            Payload::blocks(doc, vec![Block::table(table), Block::text(summary)])
                .with_exit_code(exit_code)
                .into(),
        )
    }
}
