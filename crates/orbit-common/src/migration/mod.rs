//! Forward-only schema migrations for YAML artifacts.
//!
//! Designed for the steady-state shape of schema evolution: small, mostly
//! additive `n → n+1` transforms within a single artifact lineage. Each
//! artifact kind builds a [`Plan`] once at module load (typically via
//! `OnceLock`) and reuses it on every read.
//!
//! Out of scope by design (see `docs/design/task-artifacts/4_decisions.md`
//! ADR-008): rollback (forward-fix instead), automatic write-back to disk
//! (read-time migration in memory only), and lossy cross-layout projections
//! (those belong in one-shot importers, not this framework).

use std::collections::BTreeMap;

use serde_yaml::Value;

use crate::OrbitError;

/// One step in a migration chain. Takes a YAML document at version `n`
/// and returns the same document at version `n + 1`. The step is
/// responsible for updating the `schema_version` field on the returned
/// value; [`Plan::migrate`] enforces this and bails on a regression.
pub type Step = fn(Value) -> Result<Value, OrbitError>;

/// Registered migration plan for a single artifact lineage.
pub struct Plan {
    kind: &'static str,
    target: u32,
    steps: BTreeMap<u32, Step>,
}

impl Plan {
    /// Create an empty plan whose chain terminates at `target`.
    pub fn new(kind: &'static str, target: u32) -> Self {
        Self {
            kind,
            target,
            steps: BTreeMap::new(),
        }
    }

    /// Register a step that takes the document from `from` to `from + 1`.
    /// Panics on duplicate registration or a step that would land at or
    /// past the target — both are programmer errors caught at module
    /// load when plans are typically built inside a `OnceLock`.
    pub fn add_step(mut self, from: u32, step: Step) -> Self {
        assert!(
            from < self.target,
            "{kind}: step from v{from} would land at or past target v{target}",
            kind = self.kind,
            target = self.target,
        );
        let prev = self.steps.insert(from, step);
        assert!(
            prev.is_none(),
            "{kind}: duplicate step registered from v{from}",
            kind = self.kind,
        );
        self
    }

    pub fn kind(&self) -> &'static str {
        self.kind
    }

    pub fn target(&self) -> u32 {
        self.target
    }

    /// Migrate `value` from its current `schema_version` up to `target`.
    ///
    /// Errors with [`OrbitError::Migration`] when:
    /// - the document is not a mapping or lacks `schema_version`;
    /// - the document is newer than `target` (this framework is
    ///   forward-only and won't downgrade);
    /// - a chain link is missing between the current version and target;
    /// - a step fails to advance `schema_version` by exactly one.
    pub fn migrate(&self, mut value: Value) -> Result<Value, OrbitError> {
        let mut current = self.read_version(&value, None)?;

        if current > self.target {
            return Err(OrbitError::Migration(format!(
                "{kind}: schema_version {current} is newer than supported target {target}",
                kind = self.kind,
                target = self.target,
            )));
        }

        while current < self.target {
            let step = self.steps.get(&current).ok_or_else(|| {
                OrbitError::Migration(format!(
                    "{kind}: missing migration step from v{current} (target v{target})",
                    kind = self.kind,
                    target = self.target,
                ))
            })?;

            value = step(value)?;

            let next = self.read_version(&value, Some(current))?;
            let expected = current + 1;
            if next != expected {
                return Err(OrbitError::Migration(format!(
                    "{kind}: step from v{current} produced schema_version {next}, expected {expected}",
                    kind = self.kind,
                )));
            }
            current = next;
        }

        Ok(value)
    }

    fn read_version(&self, value: &Value, after: Option<u32>) -> Result<u32, OrbitError> {
        read_schema_version(value).map_err(|err| {
            let scope = match after {
                Some(from) => format!("{kind}: after step from v{from}", kind = self.kind),
                None => self.kind.to_string(),
            };
            OrbitError::Migration(format!("{scope}: {err}"))
        })
    }
}

/// Read the top-level `schema_version` field from a YAML mapping. Public
/// so artifact-specific code can introspect a document without going
/// through a full migration.
pub fn read_schema_version(value: &Value) -> Result<u32, OrbitError> {
    let mapping = value.as_mapping().ok_or_else(|| {
        OrbitError::Migration("expected YAML mapping at document root".to_string())
    })?;

    let version = mapping
        .get(Value::String("schema_version".to_string()))
        .ok_or_else(|| OrbitError::Migration("missing schema_version field".to_string()))?;

    let raw = version.as_u64().ok_or_else(|| {
        OrbitError::Migration(format!(
            "schema_version must be a non-negative integer (got {version:?})"
        ))
    })?;

    u32::try_from(raw)
        .map_err(|_| OrbitError::Migration(format!("schema_version {raw} exceeds u32 range")))
}
