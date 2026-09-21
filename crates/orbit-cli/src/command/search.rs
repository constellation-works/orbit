use clap::{ArgAction, Args, Subcommand, ValueEnum};
use orbit_core::{
    GlobalSearchHit, GlobalSearchKind, GlobalSearchParams, OrbitError, OrbitRuntime, WorkspaceScope,
};

use crate::command::{CommandOut, Execute, Payload};

#[derive(Args)]
#[command(
    about = "Search tasks and frictions",
    subcommand_precedence_over_arg = true,
    after_help = "Forms:\n  orbit search <query>\n  orbit search reindex"
)]
pub struct SearchCommand {
    /// Free-text lexical query; multiple words need not be adjacent.
    #[arg(value_name = "query")]
    pub query: Option<String>,

    #[command(subcommand)]
    pub command: Option<SearchSubcommand>,

    /// Restrict results to one corpus kind.
    #[arg(long, value_enum, default_value_t = SearchKindArg::All, global = true)]
    pub kind: SearchKindArg,
    /// Maximum total results returned (across all kinds when --kind all;
    /// round-robin per kind to ensure fair representation).
    #[arg(long, default_value_t = 10, global = true)]
    pub limit: usize,
    /// Filter by tag (AND semantics). Applies to task and friction results.
    #[arg(long = "tag", action = ArgAction::Append, value_delimiter = ',', global = true)]
    pub tags: Vec<String>,
    /// Include normally-hidden statuses for the queried kind. Task adds
    /// done/rejected/archived; friction adds triaged/resolved.
    #[arg(long, global = true)]
    pub all: bool,
    /// Explicit per-kind status override, e.g. task:open,friction:open.
    #[arg(long, value_delimiter = ',', global = true)]
    pub status: Vec<String>,
    /// Search this registered workspace as well. Repeat or comma-separate to
    /// federate across several; every hit is labelled with the workspace it
    /// came from. Accepts a registered name, a `ws_*` ID, or an absolute
    /// checkout path. Distinct from the top-level `orbit --workspace`, which
    /// binds the whole invocation to one checkout.
    #[arg(long, action = ArgAction::Append, value_delimiter = ',', value_name = "SELECTOR")]
    pub workspaces: Vec<String>,
    /// Search every active workspace registered on this machine. Overrides
    /// `--workspaces`.
    #[arg(long)]
    pub all_workspaces: bool,
    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum SearchSubcommand {
    /// Rebuild the task search index from the task store.
    Reindex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SearchKindArg {
    Task,
    Friction,
    All,
}

impl std::fmt::Display for SearchKindArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Task => "task",
            Self::Friction => "friction",
            Self::All => "all",
        })
    }
}

impl From<SearchKindArg> for GlobalSearchKind {
    fn from(value: SearchKindArg) -> Self {
        match value {
            SearchKindArg::Task => Self::Task,
            SearchKindArg::Friction => Self::Friction,
            SearchKindArg::All => Self::All,
        }
    }
}

impl Execute for SearchCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        if matches!(self.command, Some(SearchSubcommand::Reindex)) {
            if self.query.is_some() || !self.workspaces.is_empty() || self.all_workspaces {
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
        let query = self.search_input()?;
        let response = runtime.global_search(GlobalSearchParams {
            query: Some(query),
            kind: self.kind.into(),
            limit: self.limit,
            tags: self.tags,
            all: self.all,
            status: self.status,
            path: None,
            workspaces: WorkspaceScope::from_inputs(self.workspaces, self.all_workspaces),
        })?;

        for note in &response.notes {
            eprintln!("note: {note}");
        }
        // The `--json` shape is the whole response object, not just the hits,
        // and stays that way: it is the payload [ADR-0306].
        let doc = serde_json::json!(response);
        Ok(Payload::detail_table(doc, search_table(&response.results)).into())
    }
}

impl SearchCommand {
    pub fn audit_subcommand(&self) -> String {
        let mode = match &self.command {
            Some(SearchSubcommand::Reindex) => "reindex",
            None => "query",
        };
        format!("{mode}:{}", self.kind)
    }

    fn search_input(&self) -> Result<String, OrbitError> {
        self.query
            .clone()
            .filter(|query| !query.trim().is_empty())
            .ok_or_else(|| {
                OrbitError::InvalidInput(
                    "search requires a query. Usage: orbit search <query>".into(),
                )
            })
    }
}

fn search_table(results: &[GlobalSearchHit]) -> crate::output::table::Table {
    use crate::output::table::{Column, Table};
    // A federated query labels every hit; a single-workspace one labels none,
    // so the column appears exactly when it carries information [ORB-11027].
    let federated = results.iter().any(|hit| hit.workspace.is_some());
    // Each task hit names its detail command (`orbit task show`).
    let mut columns = vec![Column::new("KIND").fixed(), Column::new("SOURCE").fixed()];
    if federated {
        columns.push(Column::new("WORKSPACE").fixed());
    }
    columns.extend([
        Column::new("ID/PATH").path(),
        Column::new("TITLE/SUMMARY"),
        Column::new("MATCH").fixed(),
    ]);
    let mut table = Table::new(columns).empty_message("no results matching the query");
    for hit in results {
        let mut row = vec![hit.kind.clone(), hit.source.clone()];
        if federated {
            row.push(
                hit.workspace
                    .as_ref()
                    .map(|workspace| workspace.name.clone())
                    .unwrap_or_default(),
            );
        }
        row.extend([
            hit.id.clone().or(hit.path.clone()).unwrap_or_default(),
            hit.title
                .clone()
                .or(hit.summary.clone())
                .unwrap_or_default(),
            match_text(hit),
        ]);
        table.add_row(row);
    }
    table
}

fn match_text(hit: &GlobalSearchHit) -> String {
    if let Some(field) = &hit.best_field {
        let score = hit.score.map(|score| format!(" score={score:.4}"));
        return format!("best={field}{}", score.unwrap_or_default());
    }
    hit.matched_by
        .as_ref()
        .map(|matched| matched.join(", "))
        .unwrap_or_default()
}
