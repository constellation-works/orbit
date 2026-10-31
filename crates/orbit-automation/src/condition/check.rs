//! The load-time checker. The `cel` crate parses (with source offsets and
//! the standard macros expanded) and evaluates, but ships no type checker, so
//! this module checks the parsed AST against the declared schema for the
//! admitted subset: literals, comparison and logic, `in`, `has()`, `size()`,
//! `startsWith`/`endsWith`/`contains`/`lowerAscii`, timestamp and duration
//! arithmetic and accessors, and comprehensions over lists. Every failure
//! names its clause and the line and column inside the expression.

use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;

use cel::IdedExpr;
use cel::common::ast::{Expr, SourceInfo};
use cel::parser::Parser;
use orbit_types::workflow::automation::{ConditionScope, ConditionTrigger};

use super::ast::{
    conjuncts, count_nodes, line_column, path_of, string_list_literal, string_literal,
};
use super::schema::{ConditionCarrier, Environment};
use super::typing::check_types;

/// Longest admitted expression source, in bytes.
pub const MAX_CONDITION_SOURCE_BYTES: usize = 4 * 1024;
/// Most AST nodes an expression may parse to, macro expansions included.
pub const MAX_CONDITION_NODES: usize = 200;
/// Deepest admitted comprehension nesting.
pub const MAX_COMPREHENSION_NESTING: usize = 2;

/// A refused condition: the clause (`trigger.condition.when`), the 1-based
/// line and column inside its expression when the clause is an expression,
/// and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConditionCheckError {
    pub clause: String,
    pub position: Option<(usize, usize)>,
    pub message: String,
}

impl fmt::Display for ConditionCheckError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.position {
            Some((line, column)) => write!(
                formatter,
                "{} at {line}:{column}: {}",
                self.clause, self.message
            ),
            None => write!(formatter, "{}: {}", self.clause, self.message),
        }
    }
}

impl std::error::Error for ConditionCheckError {}

/// One checked expression: its source and parsed AST.
#[derive(Debug, Clone)]
pub struct CheckedExpression {
    pub source: String,
    pub(crate) ast: IdedExpr,
    pub(crate) info: Arc<SourceInfo>,
}

/// The store query a task-scope condition's subjects come from: every listed
/// status (any of them) and every listed tag (all of them).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskPrefilter {
    pub statuses: Vec<String>,
    pub tags: Vec<String>,
}

/// What an expression reads beyond its subject, known at load because every
/// cross-read index is a literal.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConditionReads {
    /// `tasks['<ID>']` IDs.
    pub task_ids: BTreeSet<String>,
    /// `board.by_tag['<tag>']` tags.
    pub tags: BTreeSet<String>,
    /// `board` fields read.
    pub board: BTreeSet<&'static str>,
    /// Whether any `status_since` is read, which needs task history.
    pub status_since: bool,
    /// `steps['<id>']` step IDs a job wait reads.
    pub steps: BTreeSet<String>,
    /// Whether any `origin` is read, which needs task history.
    pub origin: bool,
    /// Whether `self` is read.
    pub self_history: bool,
}

/// A condition that passed every load-time check.
#[derive(Debug, Clone)]
pub struct CheckedCondition {
    pub scope: ConditionScope,
    pub carrier: ConditionCarrier,
    pub when: CheckedExpression,
    pub rearm: Option<CheckedExpression>,
    /// Task scope only; empty for workspace scope.
    pub prefilter: TaskPrefilter,
    pub reads: ConditionReads,
}

/// Check a condition block for `carrier`. Shape checks
/// ([`ConditionTrigger::validate`]) run first, so a refused scope or key is
/// reported before the expressions are parsed.
pub fn check_condition(
    condition: &ConditionTrigger,
    carrier: ConditionCarrier,
) -> Result<CheckedCondition, ConditionCheckError> {
    let clause = carrier.clause();
    condition.validate(clause).map_err(|error| {
        let message = error.to_string();
        match message.split_once(": ") {
            Some((key, why)) => ConditionCheckError {
                clause: key.to_string(),
                position: None,
                message: why.to_string(),
            },
            None => ConditionCheckError {
                clause: clause.to_string(),
                position: None,
                message,
            },
        }
    })?;
    let mut reads = ConditionReads::default();
    let env = Environment::Condition {
        scope: condition.scope,
        carrier,
    };
    let when = check_expression(&condition.when, &format!("{clause}.when"), env, &mut reads)?;
    let rearm = condition
        .rearm
        .as_deref()
        .map(|source| check_expression(source, &format!("{clause}.rearm"), env, &mut reads))
        .transpose()?;
    let prefilter = match condition.scope {
        ConditionScope::Task => task_prefilter(&when).map_err(|message| ConditionCheckError {
            clause: format!("{clause}.when"),
            position: Some((1, 1)),
            message,
        })?,
        _ => TaskPrefilter::default(),
    };
    Ok(CheckedCondition {
        scope: condition.scope,
        carrier,
        when,
        rearm,
        prefilter,
        reads,
    })
}

/// Parse and type-check one expression in `env`, under the size and AST
/// limits, naming `clause` in any refusal.
pub(crate) fn check_expression(
    source: &str,
    clause: &str,
    env: Environment<'_>,
    reads: &mut ConditionReads,
) -> Result<CheckedExpression, ConditionCheckError> {
    let error = |offset: Option<u32>, message: String| ConditionCheckError {
        clause: clause.to_string(),
        position: Some(line_column(source, offset.unwrap_or(0))),
        message,
    };
    if source.len() > MAX_CONDITION_SOURCE_BYTES {
        return Err(error(
            None,
            format!(
                "expression is {} bytes; the limit is {MAX_CONDITION_SOURCE_BYTES}",
                source.len()
            ),
        ));
    }
    let (ast, info) = Parser::new()
        .parse_with_source_info(source)
        .map_err(|errors| {
            let first = errors.errors.first();
            let (line, column) = first
                .map(|error| {
                    (
                        usize::try_from(error.pos.0.max(1)).unwrap_or(1),
                        usize::try_from(error.pos.1.max(1)).unwrap_or(1),
                    )
                })
                .unwrap_or((1, 1));
            ConditionCheckError {
                clause: clause.to_string(),
                position: Some((line, column)),
                message: format!(
                    "does not parse: {}",
                    first.map_or_else(|| "unknown parse error".to_string(), |e| e.msg.clone())
                ),
            }
        })?;
    let nodes = count_nodes(&ast);
    if nodes > MAX_CONDITION_NODES {
        return Err(error(
            None,
            format!("expression has {nodes} AST nodes; the limit is {MAX_CONDITION_NODES}"),
        ));
    }
    check_types(&ast, &info, env, reads)
        .map_err(|failure| error(failure.offset, failure.message))?;
    Ok(CheckedExpression {
        source: source.to_string(),
        ast,
        info: Arc::new(info),
    })
}

/// A checked `when:` guard beside a typed trigger kind.
#[derive(Debug, Clone)]
pub struct CheckedGuard {
    pub when: CheckedExpression,
    pub reads: ConditionReads,
}

/// Check a `when:` guard beside `cron`, `every_minutes`, `deliveries_landed`
/// or `state` (§11). It reads the workspace environment with no bound subject
/// and no fire history: the kind supplies identity, not edges, so `self` is
/// refused. `clause` is where the guard sits (`trigger.when`).
pub fn check_guard(
    source: &str,
    clause: &str,
    carrier: ConditionCarrier,
) -> Result<CheckedGuard, ConditionCheckError> {
    if source.trim().is_empty() {
        return Err(ConditionCheckError {
            clause: clause.to_string(),
            position: None,
            message: "must not be empty when present".into(),
        });
    }
    let mut reads = ConditionReads::default();
    let when = check_expression(
        source,
        clause,
        Environment::Condition {
            scope: ConditionScope::Workspace,
            carrier,
        },
        &mut reads,
    )?;
    if reads.self_history {
        return Err(ConditionCheckError {
            clause: clause.to_string(),
            position: Some((1, 1)),
            message: "a guard reads no `self`: the trigger kind keeps no fire history for it"
                .into(),
        });
    }
    Ok(CheckedGuard { when, reads })
}

/// Whether `expr` reads the bound subject's status (`task.status`).
fn is_subject_status(expr: &IdedExpr) -> bool {
    matches!(&expr.expr, Expr::Select(select)
        if !select.test
            && select.field == "status"
            && matches!(&select.operand.expr, Expr::Ident(name) if name == "task"))
}

/// Extract the store prefilter from the top-level conjuncts of a task-scope
/// `when` (§4): `task.status == '<s>'`, `task.status in [...]` or
/// `'<tag>' in task.tags`. A condition with none would list the whole board.
fn task_prefilter(when: &CheckedExpression) -> Result<TaskPrefilter, String> {
    let mut statuses: Option<BTreeSet<String>> = None;
    let mut tags = BTreeSet::new();
    for conjunct in conjuncts(&when.ast) {
        let Expr::Call(call) = &conjunct.expr else {
            continue;
        };
        if call.target.is_some() || call.args.len() != 2 {
            continue;
        }
        let (left, right) = (&call.args[0], &call.args[1]);
        let selected = match call.func_name.as_str() {
            "_==_" if is_subject_status(left) => string_literal(right).map(|s| vec![s.to_owned()]),
            "_==_" if is_subject_status(right) => string_literal(left).map(|s| vec![s.to_owned()]),
            "@in" if is_subject_status(left) => string_list_literal(right),
            "@in" if path_of(right).as_deref() == Some("task.tags") => {
                if let Some(tag) = string_literal(left) {
                    tags.insert(tag.to_owned());
                }
                None
            }
            _ => None,
        };
        if let Some(selected) = selected {
            let selected = selected.into_iter().collect::<BTreeSet<_>>();
            statuses = Some(match statuses {
                Some(previous) => previous.intersection(&selected).cloned().collect(),
                None => selected,
            });
        }
    }
    if statuses.is_none() && tags.is_empty() {
        return Err(
            "a task condition needs a prefilter: add a top-level `task.status == '<status>'`, \
             `task.status in ['<status>', ...]` or `'<tag>' in task.tags` conjunct joined with &&"
                .into(),
        );
    }
    if statuses.as_ref().is_some_and(BTreeSet::is_empty) {
        return Err(
            "the task status conjuncts contradict each other: no status satisfies them all, \
             so no task can match"
                .into(),
        );
    }
    Ok(TaskPrefilter {
        statuses: statuses.unwrap_or_default().into_iter().collect(),
        tags: tags.into_iter().collect(),
    })
}
