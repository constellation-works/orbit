//! Evaluation for `orbit automation explain`: a checked expression over
//! facts the caller gathered, with every top-level `&&`/`||` clause's value
//! and the operand values it read. Evaluation is pure; nothing here reads or
//! writes state.
//!
//! An evaluation that cannot complete (a cross-read ID that no longer exists,
//! a board count that could not be read) is `unknown`, never `false`
//! (`docs/design/condition-triggers/1_scope.md` §2).

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use cel::common::ast::Expr;
use cel::objects::Key;
use cel::{Context, IdedExpr, Value};
use chrono::{DateTime, Utc};
use serde::Serialize;

use super::ast::{children, conjuncts, flatten, path_of};
use super::check::CheckedExpression;

/// A condition-created task's provenance, read as `task.origin`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OriginFacts {
    pub trigger: String,
    pub fire_id: String,
    pub depth: i64,
}

/// The readable fields of one task (`task` or `tasks['<ID>']`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskFacts {
    pub id: String,
    pub title: String,
    pub status: String,
    pub tags: Vec<String>,
    pub task_type: String,
    pub priority: String,
    pub complexity: String,
    pub crew: Option<String>,
    pub parent_id: Option<String>,
    pub dependencies: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub status_since: DateTime<Utc>,
    pub origin: Option<OriginFacts>,
}

/// The `board` aggregates. A count left `None` could not be read and makes
/// any expression that reads it unknown.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BoardFacts {
    pub by_status: BTreeMap<String, i64>,
    pub by_tag: BTreeMap<String, i64>,
    pub eligible: Option<i64>,
    pub backlog: Option<i64>,
    pub in_progress: Option<i64>,
    pub blocked: Option<i64>,
    pub review: Option<i64>,
    pub open_runs: Option<i64>,
    pub drain_running: Option<bool>,
}

/// The definition's own history, read as `self`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SelfFacts {
    pub last_fired: Option<DateTime<Utc>>,
    pub last_minted: Option<DateTime<Utc>>,
    pub last_minted_task: Option<String>,
    pub fires_24h: i64,
}

/// Everything an expression reads except its subject.
#[derive(Debug, Clone)]
pub struct ConditionFacts {
    pub now: DateTime<Utc>,
    pub board: BoardFacts,
    /// The `tasks['<ID>']` cross-reads that exist; a missing ID is unknown.
    pub tasks: BTreeMap<String, TaskFacts>,
    pub self_facts: SelfFacts,
}

/// One auto-task step as a job wait reads it (`self.prev`, `steps['<id>']`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepFacts {
    /// An `AUTO_TASK_STEP_STATUSES` spelling.
    pub status: String,
    pub end_time: Option<DateTime<Utc>>,
    pub task_id: Option<String>,
}

/// Everything a job wait expression (`wait_until`, `expire_when`,
/// `spec.expire_when`) reads for one step pass
/// (`docs/design/activity-job/specs/auto-task-step.md` §2).
#[derive(Debug, Clone)]
pub struct JobWaitFacts {
    pub now: DateTime<Utc>,
    pub board: BoardFacts,
    /// `board.empty`; `None` when a count it needs could not be read.
    pub board_empty: Option<bool>,
    pub tasks: BTreeMap<String, TaskFacts>,
    pub job_started_at: DateTime<Utc>,
    pub job_source_revision: Option<String>,
    pub wait_started_at: DateTime<Utc>,
    pub status: String,
    pub task_id: Option<String>,
    pub prev: Option<StepFacts>,
    pub steps: BTreeMap<String, StepFacts>,
}

/// A three-valued result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Truth {
    True,
    False,
    /// The evaluation could not complete; the reason says why.
    Unknown(String),
}

impl Truth {
    pub fn label(&self) -> &'static str {
        match self {
            Self::True => "true",
            Self::False => "false",
            Self::Unknown(_) => "unknown",
        }
    }

    pub fn is_true(&self) -> bool {
        *self == Self::True
    }

    fn reason(&self) -> Option<String> {
        match self {
            Self::Unknown(reason) => Some(reason.clone()),
            _ => None,
        }
    }
}

/// One value an expression read.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct OperandValue {
    pub path: String,
    /// The value, or `null` with `unavailable` set when it could not be read
    /// (an absent optional field reads as `absent`).
    pub value: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<String>,
}

/// One node of the clause tree: an `&&`/`||` chain with its operands as
/// children, or a leaf clause with the values it read.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ClauseNode {
    pub text: String,
    /// `and` or `or` for a chain; absent for a leaf.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operator: Option<&'static str>,
    pub value: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub operands: Vec<OperandValue>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<ClauseNode>,
}

/// One top-level `&&` conjunct and its value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopLevelClause {
    pub text: String,
    pub value: Truth,
}

/// Binds facts as CEL variables once per explain, and the subject per call.
pub struct ConditionEvaluator {
    context: Context<'static, 'static>,
}

impl ConditionEvaluator {
    pub fn new(facts: &ConditionFacts) -> Self {
        let mut env = cel::Env::stdlib();
        // `lowerAscii` is the strings extension; nothing else from it passes
        // the checker.
        if let Err(error) = env.add_extension(cel::extensions::strings) {
            tracing::warn!(%error, "condition string functions unavailable");
        }
        let mut context = Context::with_env(Arc::new(env));
        context.add_variable_from_value("now", timestamp(facts.now));
        context.add_variable_from_value("board", board_value(&facts.board));
        context.add_variable_from_value(
            "tasks",
            map_value(
                facts
                    .tasks
                    .iter()
                    .map(|(id, task)| (id.clone(), task_value(task))),
            ),
        );
        context.add_variable_from_value("self", self_value(&facts.self_facts));
        Self { context }
    }

    /// Bind a job wait's facts [ORB-20158]. Evaluate with `truth(_, None)`.
    pub fn for_job_wait(facts: &JobWaitFacts) -> Self {
        let mut env = cel::Env::stdlib();
        if let Err(error) = env.add_extension(cel::extensions::strings) {
            tracing::warn!(%error, "condition string functions unavailable");
        }
        let mut context = Context::with_env(Arc::new(env));
        context.add_variable_from_value("now", timestamp(facts.now));
        let mut board = board_fields(&facts.board);
        if let Some(empty) = facts.board_empty {
            board.push(("empty".to_string(), Value::Bool(empty)));
        }
        context.add_variable_from_value("board", map_value(board));
        context.add_variable_from_value(
            "tasks",
            map_value(
                facts
                    .tasks
                    .iter()
                    .map(|(id, task)| (id.clone(), task_value(task))),
            ),
        );
        let mut job = vec![("started_at".to_string(), timestamp(facts.job_started_at))];
        if let Some(revision) = &facts.job_source_revision {
            job.push(("source_revision".to_string(), string(revision)));
        }
        context.add_variable_from_value("job", map_value(job));
        let mut own = vec![
            (
                "wait_started_at".to_string(),
                timestamp(facts.wait_started_at),
            ),
            ("status".to_string(), string(&facts.status)),
        ];
        if let Some(task) = &facts.task_id {
            own.push(("task_id".to_string(), string(task)));
        }
        if let Some(prev) = &facts.prev {
            own.push(("prev".to_string(), step_value(prev)));
        }
        context.add_variable_from_value("self", map_value(own));
        context.add_variable_from_value(
            "steps",
            map_value(
                facts
                    .steps
                    .iter()
                    .map(|(id, step)| (id.clone(), step_value(step))),
            ),
        );
        Self { context }
    }

    /// The whole expression's value for one subject (`None` for workspace
    /// scope).
    pub fn truth(&self, expression: &CheckedExpression, subject: Option<&TaskFacts>) -> Truth {
        let scope = self.scope(subject);
        resolve(&expression.ast, &scope)
    }

    /// Each top-level `&&` conjunct's value, for closest-miss ranking.
    pub fn top_level(
        &self,
        expression: &CheckedExpression,
        subject: Option<&TaskFacts>,
    ) -> Vec<TopLevelClause> {
        let scope = self.scope(subject);
        let text = SourceText::new(expression);
        conjuncts(&expression.ast)
            .into_iter()
            .map(|conjunct| TopLevelClause {
                text: text.of(conjunct),
                value: resolve(conjunct, &scope),
            })
            .collect()
    }

    /// The clause tree with operand values for one subject.
    pub fn clause_tree(
        &self,
        expression: &CheckedExpression,
        subject: Option<&TaskFacts>,
    ) -> ClauseNode {
        let scope = self.scope(subject);
        let text = SourceText::new(expression);
        clause_node(&expression.ast, &scope, &text)
    }

    fn scope(&self, subject: Option<&TaskFacts>) -> Context<'_, 'static> {
        let mut scope = self.context.new_inner_scope();
        if let Some(task) = subject {
            scope.add_variable_from_value("task", task_value(task));
        }
        scope
    }
}

fn resolve(expr: &IdedExpr, context: &Context<'_, '_>) -> Truth {
    match Value::resolve(expr, context) {
        Ok(Value::Bool(true)) => Truth::True,
        Ok(Value::Bool(false)) => Truth::False,
        Ok(other) => Truth::Unknown(format!("evaluated to {}", json_value(&other))),
        Err(error) => Truth::Unknown(unknown_reason(&error)),
    }
}

fn unknown_reason(error: &cel::ExecutionError) -> String {
    match error {
        cel::ExecutionError::NoSuchKey(key) => format!("'{key}' is absent or unavailable"),
        other => other.to_string(),
    }
}

fn clause_node(expr: &IdedExpr, context: &Context<'_, '_>, text: &SourceText) -> ClauseNode {
    let truth = resolve(expr, context);
    let chain = ["_&&_", "_||_"].into_iter().find(|operator| {
        matches!(&expr.expr, Expr::Call(call)
                if call.target.is_none() && call.func_name == *operator)
    });
    let (operator, children, operands) = match chain {
        Some(operator) => (
            Some(if operator == "_&&_" { "and" } else { "or" }),
            flatten(expr, operator)
                .into_iter()
                .map(|operand| clause_node(operand, context, text))
                .collect(),
            Vec::new(),
        ),
        None => (None, Vec::new(), operands(expr, context)),
    };
    ClauseNode {
        text: text.of(expr),
        operator,
        value: truth.label(),
        reason: truth.reason(),
        operands,
        children,
    }
}

/// The variables an expression may read from outside a comprehension.
const ROOTS: &[&str] = &["task", "tasks", "board", "self", "now"];

/// The maximal reads in a leaf clause, each with its value.
fn operands(expr: &IdedExpr, context: &Context<'_, '_>) -> Vec<OperandValue> {
    let mut reads = Vec::new();
    collect_reads(expr, &mut Vec::new(), &mut reads);
    let mut seen = Vec::<String>::new();
    reads
        .into_iter()
        .filter_map(|(path, read)| {
            if seen.contains(&path) {
                return None;
            }
            seen.push(path.clone());
            let (value, unavailable) = match Value::resolve(read, context) {
                Ok(value) => (json_value(&value), None),
                Err(cel::ExecutionError::NoSuchKey(_)) => {
                    (serde_json::Value::Null, Some("absent".to_string()))
                }
                Err(error) => (serde_json::Value::Null, Some(unknown_reason(&error))),
            };
            Some(OperandValue {
                path,
                value,
                unavailable,
            })
        })
        .collect()
}

fn collect_reads<'e>(
    expr: &'e IdedExpr,
    locals: &mut Vec<String>,
    reads: &mut Vec<(String, &'e IdedExpr)>,
) {
    if let Expr::Select(select) = &expr.expr
        && select.test
    {
        // `has(p.f)` reads whether `p.f` is present.
        if let Some(operand) = path_of(&select.operand)
            && is_root_read(&operand, locals)
        {
            reads.push((format!("has({operand}.{})", select.field), expr));
            return;
        }
    }
    if let Some(path) = path_of(expr)
        && is_root_read(&path, locals)
    {
        reads.push((path, expr));
        return;
    }
    if let Expr::Comprehension(comprehension) = &expr.expr {
        collect_reads(&comprehension.iter_range, locals, reads);
        collect_reads(&comprehension.accu_init, locals, reads);
        let depth = locals.len();
        locals.push(comprehension.iter_var.clone());
        locals.push(comprehension.accu_var.clone());
        collect_reads(&comprehension.loop_cond, locals, reads);
        collect_reads(&comprehension.loop_step, locals, reads);
        collect_reads(&comprehension.result, locals, reads);
        locals.truncate(depth);
        return;
    }
    for child in children(expr) {
        collect_reads(child, locals, reads);
    }
}

fn is_root_read(path: &str, locals: &[String]) -> bool {
    let root = path.split(['.', '[']).next().unwrap_or_default();
    ROOTS.contains(&root) && !locals.iter().any(|local| local == root)
}

struct SourceText {
    chars: Vec<char>,
    info: Arc<cel::common::ast::SourceInfo>,
}

impl SourceText {
    fn new(expression: &CheckedExpression) -> Self {
        Self {
            chars: expression.source.chars().collect(),
            info: Arc::clone(&expression.info),
        }
    }

    /// The source text of a subtree: from its first token to its last,
    /// widened to a call's name and to the closing brackets it opened.
    pub(crate) fn of(&self, expr: &IdedExpr) -> String {
        let Some((mut start, end)) = self.span(expr) else {
            return String::new();
        };
        let end = end.min(self.chars.len().saturating_sub(1));
        if start > end {
            return String::new();
        }
        // A global call or `has()` macro is recorded at its `(`.
        if self.chars.get(start) == Some(&'(') {
            let mut cursor = start;
            while cursor > 0 && self.chars[cursor - 1].is_whitespace() {
                cursor -= 1;
            }
            let name_end = cursor;
            while cursor > 0
                && (self.chars[cursor - 1].is_alphanumeric() || self.chars[cursor - 1] == '_')
            {
                cursor -= 1;
            }
            if cursor < name_end {
                start = cursor;
            }
        }
        let mut text: String = self.chars[start..=end].iter().collect();
        let mut open = unclosed(&text);
        let mut cursor = end + 1;
        while open > 0 && cursor < self.chars.len() {
            let character = self.chars[cursor];
            if matches!(character, ')' | ']') {
                text.push(character);
                open -= 1;
            } else if !character.is_whitespace() {
                break;
            }
            cursor += 1;
        }
        text.trim().to_string()
    }

    /// The first and last character a subtree's tokens cover. A selection is
    /// recorded at its `.`, so its field name is added after the operand.
    fn span(&self, expr: &IdedExpr) -> Option<(usize, usize)> {
        let mut range = self
            .info
            .offset_for(expr.id)
            .map(|(start, stop)| (start as usize, stop as usize));
        for child in children(expr) {
            if let Some((start, stop)) = self.span(child) {
                range = Some(match range {
                    Some((low, high)) => (low.min(start), high.max(stop)),
                    None => (start, stop),
                });
            }
        }
        if let Expr::Select(select) = &expr.expr
            && let Some((_, operand_end)) = self.span(&select.operand)
            && let Some(field_end) = self.field_end(operand_end, &select.field)
        {
            range = range.map(|(low, high)| (low, high.max(field_end)));
        }
        range
    }

    /// Where `.field` written after `operand_end` ends.
    fn field_end(&self, operand_end: usize, field: &str) -> Option<usize> {
        let skip = |mut cursor: usize| {
            while self.chars.get(cursor).is_some_and(|c| c.is_whitespace()) {
                cursor += 1;
            }
            cursor
        };
        let dot = skip(operand_end + 1);
        if self.chars.get(dot) != Some(&'.') {
            return None;
        }
        let start = skip(dot + 1);
        let length = field.chars().count();
        let written: String = self.chars.get(start..start + length)?.iter().collect();
        (written == field).then(|| start + length - 1)
    }
}

/// Brackets opened and not closed in `text`, outside string literals.
fn unclosed(text: &str) -> usize {
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for character in text.chars() {
        if let Some(open) = quote {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == open {
                quote = None;
            }
            continue;
        }
        match character {
            '\'' | '"' => quote = Some(character),
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    depth
}

fn timestamp(at: DateTime<Utc>) -> Value {
    Value::Timestamp(at.fixed_offset())
}

fn string(value: &str) -> Value {
    Value::String(Arc::new(value.to_string()))
}

fn string_list(values: &[String]) -> Value {
    Value::List(Arc::new(values.iter().map(|value| string(value)).collect()))
}

fn map_value(entries: impl IntoIterator<Item = (String, Value)>) -> Value {
    Value::from(entries.into_iter().collect::<HashMap<String, Value>>())
}

fn count_value(count: i64) -> Value {
    map_value([
        ("count".to_string(), Value::Int(count)),
        ("empty".to_string(), Value::Bool(count == 0)),
    ])
}

pub(crate) fn task_value(task: &TaskFacts) -> Value {
    let mut fields = vec![
        ("id".to_string(), string(&task.id)),
        ("title".to_string(), string(&task.title)),
        ("status".to_string(), string(&task.status)),
        ("tags".to_string(), string_list(&task.tags)),
        ("type".to_string(), string(&task.task_type)),
        ("priority".to_string(), string(&task.priority)),
        ("complexity".to_string(), string(&task.complexity)),
        ("dependencies".to_string(), string_list(&task.dependencies)),
        ("created_at".to_string(), timestamp(task.created_at)),
        ("updated_at".to_string(), timestamp(task.updated_at)),
        ("status_since".to_string(), timestamp(task.status_since)),
    ];
    if let Some(crew) = &task.crew {
        fields.push(("crew".to_string(), string(crew)));
    }
    if let Some(parent) = &task.parent_id {
        fields.push(("parent_id".to_string(), string(parent)));
    }
    if let Some(origin) = &task.origin {
        fields.push((
            "origin".to_string(),
            map_value([
                ("trigger".to_string(), string(&origin.trigger)),
                ("fire_id".to_string(), string(&origin.fire_id)),
                ("depth".to_string(), Value::Int(origin.depth)),
            ]),
        ));
    }
    map_value(fields)
}

fn board_value(board: &BoardFacts) -> Value {
    map_value(board_fields(board))
}

fn board_fields(board: &BoardFacts) -> Vec<(String, Value)> {
    let int_map = |values: &BTreeMap<String, i64>| {
        map_value(
            values
                .iter()
                .map(|(key, count)| (key.clone(), Value::Int(*count))),
        )
    };
    let mut fields = vec![
        ("by_status".to_string(), int_map(&board.by_status)),
        ("by_tag".to_string(), int_map(&board.by_tag)),
    ];
    for (name, count) in [
        ("eligible", board.eligible),
        ("backlog", board.backlog),
        ("in_progress", board.in_progress),
        ("blocked", board.blocked),
        ("review", board.review),
        ("open_runs", board.open_runs),
    ] {
        if let Some(count) = count {
            fields.push((name.to_string(), count_value(count)));
        }
    }
    if let Some(running) = board.drain_running {
        fields.push(("drain_running".to_string(), Value::Bool(running)));
    }
    fields
}

fn step_value(step: &StepFacts) -> Value {
    let mut fields = vec![("status".to_string(), string(&step.status))];
    if let Some(at) = step.end_time {
        fields.push(("end_time".to_string(), timestamp(at)));
    }
    if let Some(task) = &step.task_id {
        fields.push(("task_id".to_string(), string(task)));
    }
    map_value(fields)
}

fn self_value(facts: &SelfFacts) -> Value {
    let mut fields = vec![("fires_24h".to_string(), Value::Int(facts.fires_24h))];
    if let Some(at) = facts.last_fired {
        fields.push(("last_fired".to_string(), timestamp(at)));
    }
    if let Some(at) = facts.last_minted {
        fields.push(("last_minted".to_string(), timestamp(at)));
    }
    if let Some(task) = &facts.last_minted_task {
        fields.push(("last_minted_task".to_string(), string(task)));
    }
    map_value(fields)
}

/// A CEL value as JSON: timestamps in RFC 3339, durations in CEL's seconds
/// form (`3600s`).
pub(crate) fn json_value(value: &Value) -> serde_json::Value {
    use serde_json::Value as Json;
    match value {
        Value::Int(int) => Json::from(*int),
        Value::UInt(uint) => Json::from(*uint),
        Value::Float(float) => {
            serde_json::Number::from_f64(*float).map_or(Json::Null, Json::Number)
        }
        Value::String(text) => Json::String(text.to_string()),
        Value::Bool(flag) => Json::Bool(*flag),
        Value::Bytes(bytes) => Json::String(format!("<{} bytes>", bytes.len())),
        Value::Duration(duration) => Json::String(format!("{}s", duration.num_seconds())),
        Value::Timestamp(at) => Json::String(at.with_timezone(&Utc).to_rfc3339()),
        Value::List(items) => Json::Array(items.iter().map(json_value).collect()),
        Value::Map(map) => {
            let mut entries = map
                .map
                .iter()
                .map(|(key, value)| (key_text(key), json_value(value)))
                .collect::<Vec<_>>();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            Json::Object(entries.into_iter().collect())
        }
        Value::Null => Json::Null,
        other => Json::String(format!("{other:?}")),
    }
}

fn key_text(key: &Key) -> String {
    match key {
        Key::Int(int) => int.to_string(),
        Key::Uint(uint) => uint.to_string(),
        Key::Bool(flag) => flag.to_string(),
        Key::String(text) => text.to_string(),
    }
}
