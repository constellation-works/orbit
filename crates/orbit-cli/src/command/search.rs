use clap::{ArgAction, Args, Subcommand, ValueEnum};
use orbit_core::{
    GlobalSearchHit, GlobalSearchKind, GlobalSearchParams, OrbitError, OrbitRuntime, WorkspaceScope,
};

use crate::command::{CommandOut, Execute, Payload};

#[derive(Args)]
#[command(
    about = "Search tasks, docs, and frictions",
    subcommand_precedence_over_arg = true,
    after_help = "Forms:\n  orbit search <query>\n  orbit search similar <id>\n  orbit search path <path>"
)]
pub struct SearchCommand {
    /// Free-text query. Defaults to lexical matching unless --hybrid is set.
    #[arg(value_name = "query")]
    pub query: Option<String>,

    #[command(subcommand)]
    pub command: Option<SearchSubcommand>,

    // ADR-0179: free-text search keeps the hybrid ranker; neighbor/path modes are separate forms.
    /// Use hybrid lexical + cosine ranking for indexed task or doc fields.
    #[arg(long)]
    pub hybrid: bool,
    /// Restrict results to one corpus kind.
    #[arg(long, value_enum, default_value_t = SearchKindArg::All, global = true)]
    pub kind: SearchKindArg,
    /// Maximum total results returned (across all kinds when --kind all;
    /// round-robin per kind to ensure fair representation).
    #[arg(long, default_value_t = 10, global = true)]
    pub limit: usize,
    /// Filter by tag (AND semantics). Applies to task, doc, and friction results.
    #[arg(long = "tag", action = ArgAction::Append, value_delimiter = ',', global = true)]
    pub tags: Vec<String>,
    /// Include normally-hidden statuses for the queried kind. Task adds
    /// done/rejected/archived; friction adds triaged/resolved; doc is a no-op.
    #[arg(long, global = true)]
    pub all: bool,
    /// Explicit per-kind status override, e.g. task:open,doc:active,friction:open.
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
    /// Find cosine-neighbor tasks for a known task ID. Requires task vectors.
    Similar(SearchSimilarArgs),
    /// Filter to artifacts applicable to this filesystem path.
    Path(SearchPathArgs),
}

#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct SearchSimilarArgs {
    #[arg(value_name = "id")]
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct SearchPathArgs {
    #[arg(value_name = "path")]
    pub path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SearchKindArg {
    Task,
    Doc,
    Friction,
    All,
}

impl std::fmt::Display for SearchKindArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Task => "task",
            Self::Doc => "doc",
            Self::Friction => "friction",
            Self::All => "all",
        })
    }
}

impl From<SearchKindArg> for GlobalSearchKind {
    fn from(value: SearchKindArg) -> Self {
        match value {
            SearchKindArg::Task => Self::Task,
            SearchKindArg::Doc => Self::Doc,
            SearchKindArg::Friction => Self::Friction,
            SearchKindArg::All => Self::All,
        }
    }
}

impl Execute for SearchCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let input = self.search_input()?;
        let response = runtime.global_search(GlobalSearchParams {
            query: input.query,
            hybrid: input.hybrid,
            semantic: input.semantic,
            kind: self.kind.into(),
            limit: self.limit,
            tags: self.tags,
            all: self.all,
            status: self.status,
            path: input.path,
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
            Some(SearchSubcommand::Similar(_)) => "similar",
            Some(SearchSubcommand::Path(_)) => "path",
            None => "query",
        };
        format!("{mode}:{}", self.kind)
    }

    fn search_input(&self) -> Result<SearchInput, OrbitError> {
        match &self.command {
            Some(SearchSubcommand::Similar(args)) => {
                if self.query.as_deref().is_some_and(|query| !query.is_empty()) {
                    return Err(OrbitError::InvalidInput(
                        "`orbit search <query>` and `orbit search similar <id>` are mutually exclusive"
                            .to_string(),
                    ));
                }
                if self.hybrid {
                    return Err(OrbitError::InvalidInput(
                        "`--hybrid` only applies to `orbit search <query>`".to_string(),
                    ));
                }
                Ok(SearchInput {
                    query: None,
                    hybrid: false,
                    semantic: Some(args.id.clone()),
                    path: None,
                })
            }
            Some(SearchSubcommand::Path(args)) => {
                if self.query.as_deref().is_some_and(|query| !query.is_empty()) {
                    return Err(OrbitError::InvalidInput(
                        "`orbit search <query>` and `orbit search path <path>` are mutually exclusive"
                            .to_string(),
                    ));
                }
                if self.hybrid {
                    return Err(OrbitError::InvalidInput(
                        "`--hybrid` only applies to `orbit search <query>`".to_string(),
                    ));
                }
                Ok(SearchInput {
                    query: None,
                    hybrid: false,
                    semantic: None,
                    path: Some(args.path.clone()),
                })
            }
            None => {
                let query = self.query.clone().filter(|query| !query.trim().is_empty());
                let Some(query) = query else {
                    return Err(OrbitError::InvalidInput(
                        "search requires an input. Usage: `orbit search <query>`, `orbit search similar <id>`, or `orbit search path <path>`"
                            .to_string(),
                    ));
                };
                Ok(SearchInput {
                    query: Some(query),
                    hybrid: self.hybrid,
                    semantic: None,
                    path: None,
                })
            }
        }
    }
}

struct SearchInput {
    query: Option<String>,
    hybrid: bool,
    semantic: Option<String>,
    path: Option<String>,
}

fn search_table(results: &[GlobalSearchHit]) -> crate::output::table::Table {
    use crate::output::table::{Column, Table};
    // A federated query labels every hit; a single-workspace one labels none,
    // so the column appears exactly when it carries information [ORB-11027].
    let federated = results.iter().any(|hit| hit.workspace.is_some());
    // Each hit's kind names its own detail command (`orbit task show`
    // or `orbit docs show`).
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
