//! Search query arguments derive from the operation spec; reindex remains local maintenance.
use super::operation_args::{Invocation, augment_operation_args, input_from_matches};
use crate::command::{CommandOut, Execute, Payload};
use clap::{ArgMatches, Args, Command, FromArgMatches, Subcommand};
use orbit_common::governance::{operation::OperationVerb, search::SearchVerb};
use orbit_core::{OrbitError, OrbitRuntime};
use serde_json::Value;

pub struct SearchCommand {
    pub query: Invocation<SearchVerb>,
    pub command: Option<SearchSubcommand>,
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum SearchSubcommand {
    /// Rebuild the task search index from the task store.
    Reindex,
}

impl Args for SearchCommand {
    fn augment_args(cmd: Command) -> Command {
        let mut cmd =
            augment_operation_args(cmd.next_display_order(Some(1)), SearchVerb::Query.spec())
                .subcommand_precedence_over_arg(true)
                .mut_arg("query", |arg| arg.value_name("query"));
        // These flags have always been inherited by the maintenance subcommand.
        for name in ["kind", "limit", "tag", "all", "status"] {
            cmd = cmd.mut_arg(name, |arg| arg.global(true));
        }
        SearchSubcommand::augment_subcommands(cmd)
    }
    fn augment_args_for_update(cmd: Command) -> Command {
        Self::augment_args(cmd)
    }
}

impl FromArgMatches for SearchCommand {
    fn from_arg_matches(matches: &ArgMatches) -> Result<Self, clap::Error> {
        let spec = SearchVerb::Query.spec();
        let command = if matches.subcommand().is_some() {
            Some(SearchSubcommand::from_arg_matches(matches)?)
        } else {
            None
        };
        Ok(Self {
            query: Invocation {
                spec,
                input: input_from_matches(spec, matches),
            },
            command,
        })
    }
    fn update_from_arg_matches(&mut self, matches: &ArgMatches) -> Result<(), clap::Error> {
        *self = Self::from_arg_matches(matches)?;
        Ok(())
    }
}

impl Execute for SearchCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let Invocation { spec, input } = self.query;
        if matches!(self.command, Some(SearchSubcommand::Reindex)) {
            if input.contains_key("query")
                || input.contains_key("workspaces")
                || input.contains_key("all_workspaces")
            {
                return Err(OrbitError::InvalidInput(
                    "search reindex only applies to the current workspace and takes no query"
                        .into(),
                ));
            }
            let stats = runtime.search_reindex()?;
            return Ok(Payload::detail(
                serde_json::json!(stats),
                format!("Indexed {} chunks from {} tasks", stats.chunks, stats.tasks),
            )
            .into());
        }
        if !input
            .get("query")
            .and_then(Value::as_str)
            .is_some_and(|query| !query.trim().is_empty())
        {
            return Err(OrbitError::InvalidInput(
                "search requires a query. Usage: orbit search <query>".into(),
            ));
        }
        let value = runtime.run_tool(spec.tool_name, Value::Object(input))?;
        match spec.verb {
            SearchVerb::Query => render(&value),
        }
    }
}

impl SearchCommand {
    pub fn audit_subcommand(&self) -> String {
        let mode = if self.command.is_some() {
            "reindex".to_string()
        } else {
            self.query.audit_subcommand()
        };
        let kind = self
            .query
            .input
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("all");
        format!("{mode}:{kind}")
    }
}

fn render(value: &Value) -> CommandOut {
    if let Some(notes) = value["notes"].as_array() {
        for note in notes.iter().filter_map(Value::as_str) {
            eprintln!("note: {note}");
        }
    }
    let results = value["results"]
        .as_array()
        .ok_or_else(|| OrbitError::Execution("search response missing results".into()))?;
    Ok(Payload::detail_table(value.clone(), search_table(results)).into())
}

fn search_table(results: &[Value]) -> crate::output::table::Table {
    use crate::output::table::{Column, Table};
    // A federated query labels every hit; a single-workspace one labels none,
    // so the column appears exactly when it carries information [ORB-11027].
    let federated = results.iter().any(|hit| hit.get("workspace").is_some());
    // Each task hit names its detail command (`orbit task show`).
    let mut columns = vec![Column::new("KIND").fixed(), Column::new("SOURCE").fixed()];
    if federated {
        columns.push(Column::new("WORKSPACE").fixed());
    }
    // Partial task hits carry term counts. Show MATCH only when a result
    // has detail to display.
    let has_match = results.iter().any(|hit| !match_text(hit).is_empty());
    columns.extend([Column::new("ID/PATH").path(), Column::new("TITLE/SUMMARY")]);
    if has_match {
        columns.push(Column::new("MATCH").fixed());
    }
    let mut table = Table::new(columns).empty_message("no results matching the query");
    for hit in results {
        let mut row = vec![field(hit, "kind"), field(hit, "source")];
        if federated {
            row.push(
                hit["workspace"]["name"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            );
        }
        row.extend([
            hit.get("id")
                .or_else(|| hit.get("path"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            hit.get("title")
                .or_else(|| hit.get("summary"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        ]);
        if has_match {
            row.push(match_text(hit));
        }
        table.add_row(row);
    }
    table
}

fn field(hit: &Value, key: &str) -> String {
    hit[key].as_str().unwrap_or_default().to_string()
}

fn match_text(hit: &Value) -> String {
    if let Some(field) = hit["best_field"].as_str() {
        let score = hit["score"]
            .as_f64()
            .map(|score| format!(" score={score:.4}"));
        return format!("best={field}{}", score.unwrap_or_default());
    }
    hit["matched_by"]
        .as_array()
        .map(|matched| {
            matched
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}
