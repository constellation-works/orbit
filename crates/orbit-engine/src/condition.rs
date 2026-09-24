//! Expression-based step condition evaluator.
//!
//! Evaluates `StepCondition::Expr` strings against a `TemplateContext`.
//!
//! Expression syntax (post template resolution):
//!   `true` | `false` | `<lhs> == <rhs>` | `<lhs> != <rhs>`
//!   Combined with `&&` (AND, higher precedence) and `||` (OR).
//!
//! Examples:
//!   `"{{steps.plan.state.status}} == success"`
//!   `"{{steps.a.state.status}} == success && {{steps.b.output.match}} != false"`
//!   `"{{steps.a.state.status}} == success || {{steps.b.state.status}} == success"`

use orbit_common::OrbitError;

use crate::template::{self, TemplateContext};

/// Evaluate a boolean expression whose operands are templates. Shared between
/// v1's `StepCondition::Expr` and v2's `when:` / loop `break_when:`
/// constructs (§4.2); the grammar is documented on `evaluate_expr`.
///
/// The expression is parsed *before* rendering and each operand is rendered
/// on its own, so a rendered value (often agent output) is only ever compared,
/// never parsed: `a == a || x` in a step output cannot add an `||` branch.
pub fn evaluate_bool_expr(expr: &str, ctx: &TemplateContext) -> Result<bool, OrbitError> {
    evaluate_with(expr, &|operand| template::render(operand, ctx))
}

/// Parse and evaluate an already-resolved boolean expression.
///
/// Grammar (informal):
///   expr     = or_expr
///   or_expr  = and_expr ('||' and_expr)*
///   and_expr = atom ('&&' atom)*
///   atom     = boolean-literal | value ('==' | '!=') value
///   boolean-literal = 'true' | 'false'
///   value    = non-whitespace token (unquoted)
#[cfg(test)]
pub(crate) fn evaluate_expr(resolved: &str) -> Result<bool, OrbitError> {
    evaluate_with(resolved, &|operand| Ok(operand.to_string()))
}

type Render<'a> = dyn Fn(&str) -> Result<String, OrbitError> + 'a;

fn evaluate_with(expr: &str, render: &Render<'_>) -> Result<bool, OrbitError> {
    let mut result = false;
    for group in split_top_level(expr, "||") {
        let mut group_result = true;
        for atom in split_top_level(group, "&&") {
            group_result = group_result && evaluate_atom(atom.trim(), render)?;
        }
        result = result || group_result;
    }
    Ok(result)
}

/// Split on `delim` outside `{{ ... }}` template spans, so operators inside a
/// template expression are left for the template engine.
fn split_top_level<'a>(input: &'a str, delim: &str) -> Vec<&'a str> {
    let mut segments = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    let mut index = 0;
    while index < input.len() {
        let rest = &input[index..];
        if rest.starts_with("{{") {
            depth += 1;
            index += 2;
        } else if rest.starts_with("}}") && depth > 0 {
            depth -= 1;
            index += 2;
        } else if depth == 0 && rest.starts_with(delim) {
            segments.push(&input[start..index]);
            index += delim.len();
            start = index;
        } else {
            index += rest.chars().next().map_or(1, char::len_utf8);
        }
    }
    segments.push(&input[start..]);
    segments
}

/// Evaluate a single atom: a boolean literal, `<lhs> == <rhs>`, `<lhs> != <rhs>`,
/// or a lone template that must render to `true` or `false`.
fn evaluate_atom(atom: &str, render: &Render<'_>) -> Result<bool, OrbitError> {
    if atom == "true" {
        return Ok(true);
    }
    if atom == "false" {
        return Ok(false);
    }
    // `!=` before `==` so the `=` inside `!=` is never taken as `==`.
    for (operator, equal) in [("!=", false), ("==", true)] {
        if let [lhs, rhs] = split_top_level(atom, operator)[..] {
            let lhs = render(lhs.trim())?;
            let rhs = render(rhs.trim())?;
            return Ok((lhs.trim() == rhs.trim()) == equal);
        }
    }
    match render(atom)?.trim() {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(OrbitError::InvalidInput(format!(
            "condition atom must be 'true', 'false', or contain '==' or '!=', got: '{atom}'"
        ))),
    }
}
