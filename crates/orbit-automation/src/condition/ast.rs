//! Read-only walks over the parsed CEL AST shared by the checker and
//! evaluation: children, dotted read paths, literal values and `&&` chains.

use cel::IdedExpr;
use cel::common::ast::{Expr, LiteralValue};

/// 1-based line and column of a character offset in `source`.
pub(crate) fn line_column(source: &str, offset: u32) -> (usize, usize) {
    let mut line = 1;
    let mut column = 1;
    for (index, character) in source.chars().enumerate() {
        if index == offset as usize {
            break;
        }
        if character == '\n' {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
    }
    (line, column)
}

pub(crate) fn count_nodes(expr: &IdedExpr) -> usize {
    1 + children(expr).into_iter().map(count_nodes).sum::<usize>()
}

/// Every direct child of a node, in evaluation order.
pub(crate) fn children(expr: &IdedExpr) -> Vec<&IdedExpr> {
    match &expr.expr {
        Expr::Call(call) => call
            .target
            .as_deref()
            .into_iter()
            .chain(&call.args)
            .collect(),
        Expr::Select(select) => vec![&select.operand],
        Expr::List(list) => list.elements.iter().collect(),
        Expr::Comprehension(comprehension) => vec![
            &comprehension.iter_range,
            &comprehension.accu_init,
            &comprehension.loop_cond,
            &comprehension.loop_step,
            &comprehension.result,
        ],
        Expr::Map(map) => map
            .entries
            .iter()
            .flat_map(|entry| match &entry.expr {
                cel::common::ast::EntryExpr::MapEntry(entry) => vec![&entry.key, &entry.value],
                cel::common::ast::EntryExpr::StructField(field) => vec![&field.value],
            })
            .collect(),
        Expr::Struct(structure) => structure
            .entries
            .iter()
            .flat_map(|entry| match &entry.expr {
                cel::common::ast::EntryExpr::MapEntry(entry) => vec![&entry.key, &entry.value],
                cel::common::ast::EntryExpr::StructField(field) => vec![&field.value],
            })
            .collect(),
        Expr::Ident(_) | Expr::Literal(_) | Expr::Unspecified => Vec::new(),
    }
}

/// The dotted path an expression spells (`task.origin.trigger`,
/// `tasks['<ID>'].status`), when it is a variable with field selections
/// and literal indexes.
pub(crate) fn path_of(expr: &IdedExpr) -> Option<String> {
    match &expr.expr {
        Expr::Ident(name) => Some(name.clone()),
        Expr::Select(select) if !select.test => {
            Some(format!("{}.{}", path_of(&select.operand)?, select.field))
        }
        Expr::Call(call) if call.func_name == "_[_]" && call.target.is_none() => {
            let [target, index] = call.args.as_slice() else {
                return None;
            };
            let key = string_literal(index)?;
            Some(format!("{}['{key}']", path_of(target)?))
        }
        _ => None,
    }
}

pub(crate) fn string_literal(expr: &IdedExpr) -> Option<&str> {
    match &expr.expr {
        Expr::Literal(LiteralValue::String(value)) => Some(value.as_ref()),
        _ => None,
    }
}

/// The string literals of a list literal, when every element is one.
pub(crate) fn string_list_literal(expr: &IdedExpr) -> Option<Vec<String>> {
    match &expr.expr {
        Expr::List(list) => list
            .elements
            .iter()
            .map(|element| string_literal(element).map(str::to_owned))
            .collect(),
        _ => None,
    }
}

/// The top-level conjuncts of an expression, left to right.
pub(crate) fn conjuncts(expr: &IdedExpr) -> Vec<&IdedExpr> {
    flatten(expr, "_&&_")
}

/// The operands of a chain of one logical operator, left to right.
pub(crate) fn flatten<'e>(expr: &'e IdedExpr, operator: &str) -> Vec<&'e IdedExpr> {
    match &expr.expr {
        Expr::Call(call) if call.func_name == operator && call.target.is_none() => call
            .args
            .iter()
            .flat_map(|arg| flatten(arg, operator))
            .collect(),
        _ => vec![expr],
    }
}
