//! The type checker over the parsed AST: the admitted operators and
//! functions with their CEL overloads, schema field resolution, `has()` guard
//! tracking for optional fields, literal cross-read indexes and comprehension
//! nesting.

use std::collections::HashMap;

use cel::IdedExpr;
use cel::common::ast::{CallExpr, ComprehensionExpr, Expr, LiteralValue, SelectExpr, SourceInfo};
use orbit_types::workflow::automation::ConditionScope;

use super::ast::{path_of, string_list_literal, string_literal};
use super::check::{ConditionReads, MAX_COMPREHENSION_NESTING};
use super::schema::{self, CelType, ConditionCarrier, ObjectKind, require_status};

/// Type-check `ast` for `scope`, recording what it reads into `reads`. The
/// whole expression must be `bool`.
pub(super) fn check_types(
    ast: &IdedExpr,
    info: &SourceInfo,
    scope: ConditionScope,
    carrier: ConditionCarrier,
    reads: &mut ConditionReads,
) -> Result<(), Failure> {
    let mut checker = Checker {
        scope,
        carrier,
        info,
        locals: Vec::new(),
        guards: Vec::new(),
        reads,
        types: HashMap::new(),
        nesting: 0,
    };
    let ty = checker.check(ast)?;
    if ty != CelType::Bool {
        return checker.fail(ast, format!("expression is {}, not bool", ty.name()));
    }
    Ok(())
}

/// A refusal at a character offset of the expression.
pub(super) struct Failure {
    pub(super) offset: Option<u32>,
    pub(super) message: String,
}

struct Checker<'a> {
    scope: ConditionScope,
    carrier: ConditionCarrier,
    info: &'a SourceInfo,
    /// Comprehension variables, innermost last.
    locals: Vec<(String, CelType)>,
    /// Paths a `has()` test proves present where the current node evaluates.
    guards: Vec<String>,
    reads: &'a mut ConditionReads,
    types: HashMap<u64, CelType>,
    nesting: usize,
}

impl Checker<'_> {
    fn offset(&self, expr: &IdedExpr) -> Option<u32> {
        self.info.offset_for(expr.id).map(|(start, _)| start)
    }

    fn fail<T>(&self, expr: &IdedExpr, message: impl Into<String>) -> Result<T, Failure> {
        Err(Failure {
            offset: self.offset(expr),
            message: message.into(),
        })
    }

    fn check(&mut self, expr: &IdedExpr) -> Result<CelType, Failure> {
        let ty = self.check_node(expr)?;
        self.types.insert(expr.id, ty.clone());
        Ok(ty)
    }

    fn check_node(&mut self, expr: &IdedExpr) -> Result<CelType, Failure> {
        match &expr.expr {
            Expr::Literal(literal) => Ok(match literal {
                LiteralValue::Boolean(_) => CelType::Bool,
                LiteralValue::Bytes(_) => CelType::Bytes,
                LiteralValue::Double(_) => CelType::Double,
                LiteralValue::Int(_) => CelType::Int,
                LiteralValue::Null => CelType::Null,
                LiteralValue::String(_) => CelType::String,
                LiteralValue::UInt(_) => CelType::Uint,
            }),
            Expr::Ident(name) => self.identifier(expr, name),
            Expr::Select(select) if select.test => self.presence_test(expr, select),
            Expr::Select(select) => self.selection(expr, select),
            Expr::Call(call) => self.call(expr, call),
            Expr::List(list) => {
                if !list.optional_indices.is_empty() {
                    return self.fail(expr, "optional list elements are not admitted");
                }
                let mut element = CelType::Unknown;
                for item in &list.elements {
                    let ty = self.check(item)?;
                    element = match unify(&element, &ty) {
                        Some(unified) => unified,
                        None => {
                            return self.fail(
                                item,
                                format!(
                                    "list mixes {} and {}; a list holds one type",
                                    element.name(),
                                    ty.name()
                                ),
                            );
                        }
                    };
                }
                Ok(CelType::List(Box::new(element)))
            }
            Expr::Map(_) | Expr::Struct(_) => {
                self.fail(expr, "map and message literals are not admitted")
            }
            Expr::Comprehension(comprehension) => self.comprehension(expr, comprehension),
            Expr::Unspecified => self.fail(expr, "unsupported expression"),
        }
    }

    fn identifier(&mut self, expr: &IdedExpr, name: &str) -> Result<CelType, Failure> {
        if let Some((_, ty)) = self.locals.iter().rev().find(|(local, _)| local == name) {
            return Ok(ty.clone());
        }
        if let Some(ty) = schema::variable(self.scope, self.carrier, name) {
            return Ok(ty);
        }
        if let Some(why) = schema::unavailable_variable(self.scope, name) {
            return self.fail(expr, why);
        }
        self.fail(
            expr,
            format!(
                "unknown identifier '{name}'; a {} condition reads {}",
                self.scope.as_str(),
                match self.scope {
                    ConditionScope::Task => "task, tasks['<ID>'], board, self and now",
                    _ => "tasks['<ID>'], board, self and now",
                }
            ),
        )
    }

    fn presence_test(&mut self, expr: &IdedExpr, select: &SelectExpr) -> Result<CelType, Failure> {
        let operand = self.check(&select.operand)?;
        match operand {
            CelType::Object(ObjectKind::Tasks | ObjectKind::TagCounts) => self.fail(
                expr,
                "has() does not test a cross-read; an index that does not exist makes the \
                 evaluation unknown",
            ),
            CelType::Object(kind) => {
                if kind.field(&select.field).is_none() {
                    return self.fail(expr, unknown_field(kind, &select.field));
                }
                Ok(CelType::Bool)
            }
            CelType::Map(key, _) if *key == CelType::String => Ok(CelType::Bool),
            other => self.fail(
                expr,
                format!(
                    "has() tests a field of an object or map, not {}",
                    other.name()
                ),
            ),
        }
    }

    fn selection(&mut self, expr: &IdedExpr, select: &SelectExpr) -> Result<CelType, Failure> {
        let operand = self.check(&select.operand)?;
        match operand {
            CelType::Object(ObjectKind::Tasks) => self.fail(
                expr,
                "tasks takes a string-literal index: write tasks['<ID>']",
            ),
            CelType::Object(ObjectKind::TagCounts) => self.fail(
                expr,
                "board.by_tag takes a string-literal index: write board.by_tag['<tag>']",
            ),
            CelType::Object(kind) => {
                let Some(field) = kind.field(&select.field) else {
                    return self.fail(expr, unknown_field(kind, &select.field));
                };
                if field.optional {
                    let path = path_of(expr).unwrap_or_else(|| select.field.clone());
                    if !self.guards.contains(&path) {
                        return self.fail(
                            expr,
                            format!(
                                "`{path}` is optional; test it with has({path}) before reading it \
                                 (has({path}) && ...)"
                            ),
                        );
                    }
                }
                if kind == ObjectKind::Board {
                    self.reads.board.insert(field.name);
                }
                if field.name == "status_since" {
                    self.reads.status_since = true;
                }
                Ok(field.ty.cel_type())
            }
            CelType::Map(key, value) if *key == CelType::String => Ok(*value),
            other => self.fail(
                expr,
                format!("cannot select '{}' on {}", select.field, other.name()),
            ),
        }
    }

    fn comprehension(
        &mut self,
        expr: &IdedExpr,
        comprehension: &ComprehensionExpr,
    ) -> Result<CelType, Failure> {
        if comprehension.iter_var2.is_some() {
            return self.fail(expr, "two-variable comprehensions are not admitted");
        }
        if self.nesting >= MAX_COMPREHENSION_NESTING {
            return self.fail(
                expr,
                format!("comprehensions nest at most {MAX_COMPREHENSION_NESTING} deep"),
            );
        }
        let element = match self.check(&comprehension.iter_range)? {
            CelType::List(element) => *element,
            other => {
                return self.fail(
                    &comprehension.iter_range,
                    format!(
                        "a comprehension ranges over a bounded list such as task.tags, not {}",
                        other.name()
                    ),
                );
            }
        };
        let mut accumulator = self.check(&comprehension.accu_init)?;
        self.nesting += 1;
        let depth = self.locals.len();
        self.locals
            .push((comprehension.iter_var.clone(), element.clone()));
        self.locals
            .push((comprehension.accu_var.clone(), accumulator.clone()));
        let outcome = (|| {
            for _ in 0..2 {
                let condition = self.check(&comprehension.loop_cond)?;
                if condition != CelType::Bool {
                    return self.fail(
                        &comprehension.loop_cond,
                        "comprehension condition is not bool",
                    );
                }
                let step = self.check(&comprehension.loop_step)?;
                let Some(unified) = unify(&accumulator, &step) else {
                    return self.fail(
                        &comprehension.loop_step,
                        format!(
                            "comprehension step is {}, not {}",
                            step.name(),
                            accumulator.name()
                        ),
                    );
                };
                if unified == accumulator {
                    break;
                }
                accumulator = unified;
                if let Some(last) = self.locals.last_mut() {
                    last.1 = accumulator.clone();
                }
            }
            self.check(&comprehension.result)
        })();
        self.locals.truncate(depth);
        self.nesting -= 1;
        outcome
    }

    fn call(&mut self, expr: &IdedExpr, call: &CallExpr) -> Result<CelType, Failure> {
        let name = call.func_name.as_str();
        if let Some(target) = &call.target {
            return self.member_call(expr, name, target, &call.args);
        }
        let args = call.args.as_slice();
        match name {
            "_&&_" | "_||_" => {
                let [left, right] = args else {
                    return self.fail(expr, "logical operator takes two operands");
                };
                let left_ty = self.check(left)?;
                let facts = if name == "_&&_" {
                    facts_if_true(left)
                } else {
                    facts_if_false(left)
                };
                let right_ty = self.with_guards(facts, |checker| checker.check(right))?;
                for (operand, ty) in [(left, left_ty), (right, right_ty)] {
                    if ty != CelType::Bool {
                        return self.fail(
                            operand,
                            format!("operand of && or || is {}, not bool", ty.name()),
                        );
                    }
                }
                Ok(CelType::Bool)
            }
            "!_" | "@not_strictly_false" => {
                let [operand] = args else {
                    return self.fail(expr, "! takes one operand");
                };
                match self.check(operand)? {
                    CelType::Bool => Ok(CelType::Bool),
                    other => self.fail(operand, format!("! applies to bool, not {}", other.name())),
                }
            }
            "-_" => {
                let [operand] = args else {
                    return self.fail(expr, "- takes one operand");
                };
                match self.check(operand)? {
                    ty @ (CelType::Int | CelType::Double | CelType::Duration) => Ok(ty),
                    other => self.fail(operand, format!("cannot negate {}", other.name())),
                }
            }
            "_?_:_" => {
                let [condition, then, otherwise] = args else {
                    return self.fail(expr, "?: takes three operands");
                };
                if self.check(condition)? != CelType::Bool {
                    return self.fail(condition, "?: condition is not bool");
                }
                let then_ty =
                    self.with_guards(facts_if_true(condition), |checker| checker.check(then))?;
                let otherwise_ty = self.with_guards(facts_if_false(condition), |checker| {
                    checker.check(otherwise)
                })?;
                match unify(&then_ty, &otherwise_ty) {
                    Some(ty) => Ok(ty),
                    None => self.fail(
                        expr,
                        format!(
                            "?: branches are {} and {}",
                            then_ty.name(),
                            otherwise_ty.name()
                        ),
                    ),
                }
            }
            "_==_" | "_!=_" => {
                let [left, right] = args else {
                    return self.fail(expr, "comparison takes two operands");
                };
                let left_ty = self.check(left)?;
                let right_ty = self.check(right)?;
                if !compatible(&left_ty, &right_ty) {
                    return self.fail(
                        expr,
                        format!("cannot compare {} with {}", left_ty.name(), right_ty.name()),
                    );
                }
                self.check_status_literals(left, right)?;
                self.check_status_literals(right, left)?;
                Ok(CelType::Bool)
            }
            "_<_" | "_<=_" | "_>_" | "_>=_" => {
                let [left, right] = args else {
                    return self.fail(expr, "comparison takes two operands");
                };
                let left_ty = self.check(left)?;
                let right_ty = self.check(right)?;
                let ordered = (left_ty.is_numeric() && right_ty.is_numeric())
                    || (left_ty == right_ty
                        && matches!(
                            left_ty,
                            CelType::String
                                | CelType::Bytes
                                | CelType::Bool
                                | CelType::Timestamp
                                | CelType::Duration
                        ));
                if !ordered {
                    return self.fail(
                        expr,
                        format!("cannot order {} and {}", left_ty.name(), right_ty.name()),
                    );
                }
                Ok(CelType::Bool)
            }
            "_+_" | "_-_" | "_*_" | "_/_" | "_%_" => {
                let [left, right] = args else {
                    return self.fail(expr, "arithmetic takes two operands");
                };
                let left_ty = self.check(left)?;
                let right_ty = self.check(right)?;
                match arithmetic(name, &left_ty, &right_ty) {
                    Some(ty) => Ok(ty),
                    None => self.fail(
                        expr,
                        format!(
                            "operator {} does not apply to {} and {}",
                            operator_symbol(name),
                            left_ty.name(),
                            right_ty.name()
                        ),
                    ),
                }
            }
            "_[_]" => {
                let [target, index] = args else {
                    return self.fail(expr, "index takes two operands");
                };
                self.index(expr, target, index)
            }
            "@in" => {
                let [element, collection] = args else {
                    return self.fail(expr, "in takes two operands");
                };
                let element_ty = self.check(element)?;
                let collection_ty = self.check(collection)?;
                let admitted = match &collection_ty {
                    CelType::List(item) => compatible(&element_ty, item),
                    CelType::Map(key, _) => compatible(&element_ty, key),
                    _ => false,
                };
                if !admitted {
                    return self.fail(
                        expr,
                        format!(
                            "cannot test {} in {}",
                            element_ty.name(),
                            collection_ty.name()
                        ),
                    );
                }
                if is_status_read(element, &self.types)
                    && let Some(literals) = string_list_literal(collection)
                {
                    for literal in literals {
                        require_status(&literal).map_err(|message| Failure {
                            offset: self.offset(collection),
                            message,
                        })?;
                    }
                }
                Ok(CelType::Bool)
            }
            "size" => {
                let [operand] = args else {
                    return self.fail(expr, "size() takes one argument");
                };
                let ty = self.check(operand)?;
                self.sized(expr, &ty)
            }
            "timestamp" | "duration" => {
                let [operand] = args else {
                    return self.fail(expr, format!("{name}() takes one string argument"));
                };
                if self.check(operand)? != CelType::String {
                    return self.fail(operand, format!("{name}() takes a string"));
                }
                if string_literal(operand).is_some()
                    && let Err(error) = cel::Value::resolve(expr, &cel::Context::default())
                {
                    return self.fail(operand, format!("invalid {name} literal: {error}"));
                }
                Ok(if name == "timestamp" {
                    CelType::Timestamp
                } else {
                    CelType::Duration
                })
            }
            "matches" => self.fail(
                expr,
                "matches() is not admitted: conditions take no regular expressions",
            ),
            _ => self.fail(
                expr,
                format!("function '{name}' is not admitted in a condition"),
            ),
        }
    }

    fn member_call(
        &mut self,
        expr: &IdedExpr,
        name: &str,
        target: &IdedExpr,
        args: &[IdedExpr],
    ) -> Result<CelType, Failure> {
        if name == "matches" {
            return self.fail(
                expr,
                "matches() is not admitted: conditions take no regular expressions",
            );
        }
        let receiver = self.check(target)?;
        let arg_types = args
            .iter()
            .map(|arg| self.check(arg))
            .collect::<Result<Vec<_>, _>>()?;
        let result = match (name, &receiver, arg_types.as_slice()) {
            ("size", _, []) => return self.sized(expr, &receiver),
            ("startsWith" | "endsWith" | "contains", CelType::String, [CelType::String]) => {
                CelType::Bool
            }
            ("lowerAscii", CelType::String, []) => CelType::String,
            (
                "getFullYear" | "getMonth" | "getDayOfYear" | "getDayOfMonth" | "getDate"
                | "getDayOfWeek" | "getHours" | "getMinutes" | "getSeconds" | "getMilliseconds",
                CelType::Timestamp,
                [] | [CelType::String],
            ) => CelType::Int,
            (
                "getHours" | "getMinutes" | "getSeconds" | "getMilliseconds",
                CelType::Duration,
                [],
            ) => CelType::Int,
            (
                "size" | "startsWith" | "endsWith" | "contains" | "lowerAscii" | "getFullYear"
                | "getMonth" | "getDayOfYear" | "getDayOfMonth" | "getDate" | "getDayOfWeek"
                | "getHours" | "getMinutes" | "getSeconds" | "getMilliseconds",
                _,
                _,
            ) => {
                return self.fail(
                    expr,
                    format!(
                        "{name}() does not apply to {}({})",
                        receiver.name(),
                        arg_types
                            .iter()
                            .map(CelType::name)
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                );
            }
            _ => {
                return self.fail(
                    expr,
                    format!("function '{name}' is not admitted in a condition"),
                );
            }
        };
        Ok(result)
    }

    fn sized(&self, expr: &IdedExpr, ty: &CelType) -> Result<CelType, Failure> {
        match ty {
            CelType::String | CelType::Bytes | CelType::List(_) | CelType::Map(_, _) => {
                Ok(CelType::Int)
            }
            other => self.fail(expr, format!("size() does not apply to {}", other.name())),
        }
    }

    fn index(
        &mut self,
        expr: &IdedExpr,
        target: &IdedExpr,
        index: &IdedExpr,
    ) -> Result<CelType, Failure> {
        let target_ty = self.check(target)?;
        match target_ty {
            CelType::Object(ObjectKind::Tasks) => {
                let Some(id) = string_literal(index) else {
                    return self.fail(
                        index,
                        "computed cross-read index: tasks[...] takes a string literal, such as \
                         tasks['<ID>']",
                    );
                };
                if id.trim().is_empty() {
                    return self.fail(index, "tasks[...] takes a task ID");
                }
                self.check(index)?;
                self.reads.task_ids.insert(id.to_owned());
                Ok(CelType::Object(ObjectKind::Task))
            }
            CelType::Object(ObjectKind::TagCounts) => {
                let Some(tag) = string_literal(index) else {
                    return self.fail(
                        index,
                        "computed cross-read index: board.by_tag[...] takes a string literal, \
                         such as board.by_tag['release']",
                    );
                };
                self.check(index)?;
                self.reads.tags.insert(tag.to_owned());
                Ok(CelType::Int)
            }
            CelType::List(element) => match self.check(index)? {
                CelType::Int | CelType::Uint => Ok(*element),
                other => self.fail(index, format!("a list index is int, not {}", other.name())),
            },
            CelType::Map(key, value) => {
                let index_ty = self.check(index)?;
                if !compatible(&index_ty, &key) {
                    return self.fail(
                        index,
                        format!(
                            "a {} index is {}, not {}",
                            target_ty_name(&key, &value),
                            key.name(),
                            index_ty.name()
                        ),
                    );
                }
                if path_of(target).as_deref() == Some("board.by_status")
                    && let Some(status) = string_literal(index)
                {
                    require_status(status).map_err(|message| Failure {
                        offset: self.offset(index),
                        message,
                    })?;
                }
                Ok(*value)
            }
            other => self.fail(expr, format!("cannot index {}", other.name())),
        }
    }

    /// Refuse a misspelled status where `subject` reads a task status and
    /// `other` is a string literal.
    fn check_status_literals(&self, subject: &IdedExpr, other: &IdedExpr) -> Result<(), Failure> {
        if is_status_read(subject, &self.types)
            && let Some(literal) = string_literal(other)
        {
            require_status(literal).map_err(|message| Failure {
                offset: self.offset(other),
                message,
            })?;
        }
        Ok(())
    }

    fn with_guards<T>(
        &mut self,
        facts: Vec<String>,
        body: impl FnOnce(&mut Self) -> Result<T, Failure>,
    ) -> Result<T, Failure> {
        let depth = self.guards.len();
        self.guards.extend(facts);
        let outcome = body(self);
        self.guards.truncate(depth);
        outcome
    }
}

/// Paths a true result proves present: `has(p)` conjuncts.
fn facts_if_true(expr: &IdedExpr) -> Vec<String> {
    match &expr.expr {
        Expr::Select(select) if select.test => path_of(&select.operand)
            .map(|operand| vec![format!("{operand}.{}", select.field)])
            .unwrap_or_default(),
        Expr::Call(call) if call.target.is_none() && call.func_name == "_&&_" => {
            call.args.iter().flat_map(facts_if_true).collect()
        }
        Expr::Call(call) if call.target.is_none() && call.func_name == "!_" => {
            call.args.iter().flat_map(facts_if_false).collect()
        }
        _ => Vec::new(),
    }
}

/// Paths a false result proves present: `!has(p)` disjuncts.
fn facts_if_false(expr: &IdedExpr) -> Vec<String> {
    match &expr.expr {
        Expr::Call(call) if call.target.is_none() && call.func_name == "_||_" => {
            call.args.iter().flat_map(facts_if_false).collect()
        }
        Expr::Call(call) if call.target.is_none() && call.func_name == "!_" => {
            call.args.iter().flat_map(facts_if_true).collect()
        }
        _ => Vec::new(),
    }
}

/// Whether `expr` reads the `status` of any task object.
fn is_status_read(expr: &IdedExpr, types: &HashMap<u64, CelType>) -> bool {
    matches!(&expr.expr, Expr::Select(select)
        if !select.test
            && select.field == "status"
            && types.get(&select.operand.id) == Some(&CelType::Object(ObjectKind::Task)))
}

fn unknown_field(kind: ObjectKind, field: &str) -> String {
    let fields = kind
        .fields()
        .iter()
        .map(|field| field.name)
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "unknown field '{field}' on {}; its fields are {fields}",
        kind.name()
    )
}

fn operator_symbol(name: &str) -> &str {
    name.trim_matches('_')
}

fn target_ty_name(key: &CelType, value: &CelType) -> String {
    CelType::Map(Box::new(key.clone()), Box::new(value.clone())).name()
}

fn compatible(left: &CelType, right: &CelType) -> bool {
    match (left, right) {
        (CelType::Unknown, _) | (_, CelType::Unknown) => true,
        (left, right) if left.is_numeric() && right.is_numeric() => true,
        (CelType::List(left), CelType::List(right)) => compatible(left, right),
        (CelType::Map(left_key, left_value), CelType::Map(right_key, right_value)) => {
            compatible(left_key, right_key) && compatible(left_value, right_value)
        }
        (left, right) => left == right,
    }
}

fn unify(left: &CelType, right: &CelType) -> Option<CelType> {
    match (left, right) {
        (CelType::Unknown, other) | (other, CelType::Unknown) => Some(other.clone()),
        (CelType::List(left), CelType::List(right)) => {
            Some(CelType::List(Box::new(unify(left, right)?)))
        }
        (CelType::Map(left_key, left_value), CelType::Map(right_key, right_value)) => {
            Some(CelType::Map(
                Box::new(unify(left_key, right_key)?),
                Box::new(unify(left_value, right_value)?),
            ))
        }
        (left, right) if left == right => Some(left.clone()),
        _ => None,
    }
}

/// CEL's arithmetic overloads: no implicit numeric conversion.
fn arithmetic(operator: &str, left: &CelType, right: &CelType) -> Option<CelType> {
    use CelType::{Bytes, Double, Duration, Int, List, String, Timestamp, Uint};
    match (operator, left, right) {
        ("_+_" | "_-_" | "_*_" | "_/_", Int, Int) => Some(Int),
        ("_+_" | "_-_" | "_*_" | "_/_", Uint, Uint) => Some(Uint),
        ("_+_" | "_-_" | "_*_" | "_/_", Double, Double) => Some(Double),
        ("_%_", Int, Int) => Some(Int),
        ("_%_", Uint, Uint) => Some(Uint),
        ("_+_", String, String) => Some(String),
        ("_+_", Bytes, Bytes) => Some(Bytes),
        ("_+_", List(_), List(_)) => unify(left, right),
        ("_+_", Timestamp, Duration) | ("_+_", Duration, Timestamp) => Some(Timestamp),
        ("_+_" | "_-_", Duration, Duration) => Some(Duration),
        ("_-_", Timestamp, Timestamp) => Some(Duration),
        ("_-_", Timestamp, Duration) => Some(Timestamp),
        _ => None,
    }
}
