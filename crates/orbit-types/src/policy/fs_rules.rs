//! Compiled evaluation of a resolved profile's filesystem rules.
//!
//! Two layers ask the same question about a path: [`PolicyDef::check_path`]
//! answers it for a single request, and the process-boundary sandbox in
//! `orbit-exec` answers it for every path in a workspace while compiling a
//! kernel ruleset. Both go through this type, so last-match-wins precedence
//! and the reported `matched_rule` are defined once instead of drifting
//! between the request-time deny and the enforced grant.
//!
//! Compiling is also what makes the sandbox's whole-tree walk affordable:
//! matching a rule set through [`match_glob`](crate::policy::match_glob)
//! rebuilds a regex per call, which is fine for one path and quadratic-feeling
//! for thousands.

use regex::Regex;

use crate::policy::PolicyError;
use crate::policy::glob::{compile_glob_regex, normalize_glob_path};
use crate::policy::policy_def::FsCheckResult;

/// `matched_rule` reported when rules exist but none matched the path.
const NO_MATCHING_RULE: &str = "<no matching rule>";
/// `matched_rule` reported when the rule set cannot allow anything.
const EMPTY_RULESET: &str = "[]";

/// A resolved profile's rule set for one operation, compiled for reuse.
#[derive(Debug)]
pub struct CompiledFsRules {
    rules: Vec<CompiledFsRule>,
}

#[derive(Debug)]
struct CompiledFsRule {
    negated: bool,
    /// The rule as written, minus any `!` prefix — the deny form reports the
    /// bare pattern while the allow form reports the rule itself.
    pattern: String,
    matcher: Regex,
}

impl CompiledFsRules {
    /// Compile `rules` as produced by
    /// [`PolicyDef::effective_profile`](crate::policy::PolicyDef::effective_profile).
    ///
    /// `label` names the rule set in error messages.
    pub fn compile(rules: &[String], label: &str) -> Result<Self, PolicyError> {
        let compiled = rules
            .iter()
            .map(|rule| CompiledFsRule::compile(rule, label))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { rules: compiled })
    }

    /// True when no path can ever be allowed, because the rule set is empty or
    /// contains only exclusions. A caller walking a tree can stop immediately.
    pub fn grants_nothing(&self) -> bool {
        self.rules.iter().all(|rule| rule.negated)
    }

    /// True when an exclusion could carve a denied path out of an allowed
    /// subtree. Without one, an allowed subtree needs no further inspection.
    pub fn has_exclusion(&self) -> bool {
        self.rules.iter().any(|rule| rule.negated)
    }

    /// Decide `path`, reporting the rule that settled it.
    ///
    /// `path` is normalized here, so callers may pass either the `./a/b` or
    /// `a/b` spelling of a workspace-relative path.
    pub fn evaluate(&self, path: &str) -> Result<FsCheckResult, PolicyError> {
        let normalized = normalize_glob_path(path)?;
        Ok(self.evaluate_normalized(&normalized))
    }

    /// Decide `path`, discarding the explanation.
    pub fn allows(&self, path: &str) -> Result<bool, PolicyError> {
        Ok(self.evaluate(path)?.allowed)
    }

    /// Last matching rule wins: a later exclusion overrides an earlier grant,
    /// which is how `denyRead` is appended to a profile's own read rules.
    fn evaluate_normalized(&self, normalized: &str) -> FsCheckResult {
        let mut decision = None;
        for rule in &self.rules {
            if rule.matcher.is_match(normalized) {
                decision = Some(rule.decision());
            }
        }

        decision.unwrap_or_else(|| FsCheckResult {
            allowed: false,
            matched_rule: if self.grants_nothing() {
                EMPTY_RULESET.to_string()
            } else {
                NO_MATCHING_RULE.to_string()
            },
        })
    }
}

impl CompiledFsRule {
    fn compile(rule: &str, label: &str) -> Result<Self, PolicyError> {
        let (negated, body) = rule
            .strip_prefix('!')
            .map(|rest| (true, rest))
            .unwrap_or((false, rule));
        let mut pattern = body.replace('\\', "/");
        while let Some(stripped) = pattern.strip_prefix("./") {
            pattern = stripped.to_string();
        }
        if pattern.is_empty() {
            pattern = ".".to_string();
        }

        let matcher = compile_glob_regex(&pattern).map_err(|error| {
            PolicyError::Invalid(format!(
                "{label} rule `{rule}` is not a valid filesystem glob: {error}"
            ))
        })?;
        Ok(Self {
            negated,
            pattern,
            matcher,
        })
    }

    fn decision(&self) -> FsCheckResult {
        FsCheckResult {
            allowed: !self.negated,
            matched_rule: self.pattern.clone(),
        }
    }
}
