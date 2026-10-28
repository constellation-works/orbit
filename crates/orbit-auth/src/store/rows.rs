//! Decoding token rows from SQLite.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use rusqlite::Row;

use crate::scope::{LeaseTokenRecord, LeaseTokenScope};

/// A row read from `lease_tokens`; decoding failures surface as a corrupt
/// store rather than a SQLite error.
pub(super) type DecodedRow = Result<LeaseTokenRecord, OrbitError>;

pub(super) fn token_from_row(row: &Row<'_>) -> rusqlite::Result<DecodedRow> {
    token_from_offset_row(row, 0)
}

pub(super) fn token_from_offset_row(row: &Row<'_>, offset: usize) -> rusqlite::Result<DecodedRow> {
    let text = |index: usize| row.get::<_, String>(offset + index);
    let optional_text = |index: usize| row.get::<_, Option<String>>(offset + index);
    let stored = StoredToken {
        token_id: text(0)?,
        principal: text(1)?,
        principal_class: text(2)?,
        workspaces: text(3)?,
        verbs: text(4)?,
        max_priority: text(5)?,
        hard: text(6)?,
        created_at: text(7)?,
        expires_at: text(8)?,
        last_used_at: optional_text(9)?,
        revoked_at: optional_text(10)?,
    };
    Ok(stored.decode())
}

/// A `lease_tokens` row as stored text.
struct StoredToken {
    token_id: String,
    principal: String,
    principal_class: String,
    workspaces: String,
    verbs: String,
    max_priority: String,
    hard: String,
    created_at: String,
    expires_at: String,
    last_used_at: Option<String>,
    revoked_at: Option<String>,
}

impl StoredToken {
    fn decode(self) -> DecodedRow {
        let token_id = self.token_id;
        let corrupt = |detail: String| {
            OrbitError::Store(format!(
                "lease token store row {token_id} is unreadable: {detail}"
            ))
        };
        let parse_time = |raw: &str| {
            DateTime::parse_from_rfc3339(raw)
                .map(|at| at.with_timezone(&Utc))
                .map_err(|error| corrupt(format!("timestamp '{raw}': {error}")))
        };
        let scope = LeaseTokenScope {
            principal: self.principal,
            principal_class: self
                .principal_class
                .parse()
                .map_err(|error: OrbitError| corrupt(error.to_string()))?,
            workspaces: self.workspaces.split(',').map(str::to_string).collect(),
            verbs: self
                .verbs
                .split(',')
                .map(str::parse)
                .collect::<Result<_, OrbitError>>()
                .map_err(|error| corrupt(error.to_string()))?,
            max_priority: self
                .max_priority
                .parse()
                .map_err(|error: OrbitError| corrupt(error.to_string()))?,
            hard: self
                .hard
                .parse()
                .map_err(|error: OrbitError| corrupt(error.to_string()))?,
        };
        let created_at = parse_time(&self.created_at)?;
        let expires_at = parse_time(&self.expires_at)?;
        let last_used_at = self.last_used_at.as_deref().map(parse_time).transpose()?;
        let revoked_at = self.revoked_at.as_deref().map(parse_time).transpose()?;
        Ok(LeaseTokenRecord {
            token_id,
            scope,
            created_at,
            expires_at,
            last_used_at,
            revoked_at,
        })
    }
}
