use orbit_common::OrbitError;
use serde_json::Value;
use std::cell::Cell;
use std::path::Path;
use std::time::{Duration, Instant};
/// Bounded wall-clock budgets for host Git children.
///
/// Heavyweight mutations get their own defaults; every request still carries a
/// finite `ExecRequest.timeout_ms`. Activity input may overlay these values
/// through `git_timeout_ms` / `git_timeouts` without changing hook or
/// environment policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct GitTimeoutBudget {
    pub default_ms: u64,
    pub fetch_ms: u64,
    pub worktree_add_ms: u64,
    pub rebase_ms: u64,
}

impl GitTimeoutBudget {
    pub const MIN_MS: u64 = 1;
    pub const MAX_MS: u64 = 600_000;

    pub const DEFAULT: Self = Self {
        default_ms: 30_000,
        fetch_ms: 60_000,
        worktree_add_ms: 120_000,
        rebase_ms: 120_000,
    };

    pub(crate) fn from_input(input: &Value) -> Result<Self, OrbitError> {
        let mut budget = Self::DEFAULT;
        if let Some(value) = input.get("git_timeout_ms") {
            let ms = parse_timeout_ms(value, "git_timeout_ms")?;
            budget = Self {
                default_ms: ms,
                fetch_ms: ms,
                worktree_add_ms: ms,
                rebase_ms: ms,
            };
        }
        if let Some(timeouts) = input.get("git_timeouts") {
            let Some(map) = timeouts.as_object() else {
                return Err(OrbitError::InvalidInput(
                    "input.git_timeouts must be an object".to_string(),
                ));
            };
            for (key, value) in map {
                let field = format!("git_timeouts.{key}");
                let ms = parse_timeout_ms(value, &field)?;
                match key.as_str() {
                    "default" => budget.default_ms = ms,
                    "fetch" => budget.fetch_ms = ms,
                    "worktree_add" => budget.worktree_add_ms = ms,
                    "rebase" => budget.rebase_ms = ms,
                    other => {
                        return Err(OrbitError::InvalidInput(format!(
                            "unknown git_timeouts key '{other}'; expected default, fetch, worktree_add, or rebase"
                        )));
                    }
                }
            }
        }
        Ok(budget)
    }

    pub(crate) fn current() -> Self {
        GIT_TIMEOUT_BUDGET.with(Cell::get)
    }

    pub(crate) fn timeout_for(self, args: &[&str]) -> u64 {
        // Git's global `-c key=value` may precede the operation (notably
        // `-c core.editor=true rebase --continue`).
        let mut operation = args;
        while let ["-c", _, rest @ ..] = operation {
            operation = rest;
        }
        match operation {
            ["fetch", ..] => self.fetch_ms,
            ["worktree", "add", ..] => self.worktree_add_ms,
            ["rebase", ..] => self.rebase_ms,
            _ => self.default_ms,
        }
    }
}

thread_local! {
    pub(super) static GIT_DEADLINE: Cell<Option<Instant>> = const { Cell::new(None) };
    static GIT_TIMEOUT_BUDGET: Cell<GitTimeoutBudget> =
        const { Cell::new(GitTimeoutBudget::DEFAULT) };
}

/// Shared deadline for the read/fetch phase of a fresh preparation. Rewrites
/// retain their ordinary timeout and owned-rebase cleanup budget.
pub(crate) struct GitDeadlineGuard(Option<Instant>);

impl GitDeadlineGuard {
    pub(crate) fn install(remaining: Duration) -> Self {
        Self(GIT_DEADLINE.with(|slot| slot.replace(Some(Instant::now() + remaining))))
    }
}

impl Drop for GitDeadlineGuard {
    fn drop(&mut self) {
        GIT_DEADLINE.with(|slot| slot.set(self.0));
    }
}

pub(super) fn command_timeout(args: &[&str]) -> Result<u64, OrbitError> {
    let timeout = GitTimeoutBudget::current().timeout_for(args);
    GIT_DEADLINE.with(|slot| match slot.get() {
        Some(deadline) => deadline
            .checked_duration_since(Instant::now())
            .map(|remaining| timeout.min(remaining.as_millis().max(1) as u64))
            .ok_or_else(|| {
                OrbitError::Execution(
                    "prepared_base_refresh_refused: fresh preparation deadline exhausted"
                        .to_string(),
                )
            }),
        None => Ok(timeout),
    })
}

/// Restores the previous Git timeout budget when the scope exits.
pub(crate) struct GitTimeoutBudgetGuard {
    previous: GitTimeoutBudget,
}

impl GitTimeoutBudgetGuard {
    pub(crate) fn install(budget: GitTimeoutBudget) -> Self {
        let previous = GIT_TIMEOUT_BUDGET.with(|cell| cell.replace(budget));
        Self { previous }
    }
}

impl Drop for GitTimeoutBudgetGuard {
    fn drop(&mut self) {
        GIT_TIMEOUT_BUDGET.with(|cell| cell.set(self.previous));
    }
}

fn parse_timeout_ms(value: &Value, field: &str) -> Result<u64, OrbitError> {
    let Some(ms) = value.as_u64() else {
        return Err(OrbitError::InvalidInput(format!(
            "{field} must be an integer number of milliseconds between {} and {}",
            GitTimeoutBudget::MIN_MS,
            GitTimeoutBudget::MAX_MS
        )));
    };
    if !(GitTimeoutBudget::MIN_MS..=GitTimeoutBudget::MAX_MS).contains(&ms) {
        return Err(OrbitError::InvalidInput(format!(
            "{field} must be between {} and {} ms, got {ms}",
            GitTimeoutBudget::MIN_MS,
            GitTimeoutBudget::MAX_MS
        )));
    }
    Ok(ms)
}

/// Captured Git child result, including timeout vs ordinary failure.
pub(crate) fn git_timeout_error(
    current_dir: &Path,
    args: &[&str],
    timeout_ms: u64,
    stderr: &str,
) -> OrbitError {
    timeout_recovery_error(
        timeout_ms,
        format!(
            "git {} timed out after {timeout_ms}ms in '{}': {}",
            args.join(" "),
            current_dir.display(),
            stderr.trim()
        ),
    )
}

/// An execution failure caused by a Git deadline, worded by the caller. The
/// timeout class travels in the variant, so recovery guidance never has to
/// recognize it from the text.
pub(crate) fn timeout_recovery_error(timeout_ms: u64, message: String) -> OrbitError {
    OrbitError::ExecutionTimeout {
        timeout_ms,
        message,
    }
}
