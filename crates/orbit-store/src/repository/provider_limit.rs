//! Concrete repository for this host's provider usage limits
//! (`provider_limit_observations` for the default login,
//! `provider_account_limit_observations` for named crew accounts) and its
//! crew outages (`provider_crew_outages`).
//!
//! Reached only through [`Snapshot::provider_limit`] (read-only) and
//! [`Tx::provider_limit`] (read-write inside [`crate::Store::write`]).
//! Timestamps are stored as fixed-width UTC RFC3339 text (microseconds, `Z`),
//! so SQLite orders them by comparing the text. Every write keeps the newest
//! row of its key: an older observation, an older reading or a shorter outage
//! never replaces a newer one, because concurrent drains write the same rows.

use std::marker::PhantomData;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use orbit_types::telemetry::{ProviderLimitObservation, ProviderLimitSource};
use orbit_types::workflow::ProviderCrewOutage;
use rusqlite::{Connection, params};

use crate::driver::sqlite::codec::stamp;
use crate::store::{Snapshot, Tx};

const NAMED_CREW_READING_SQL: ReadingSql = ReadingSql {
    select: "SELECT model_scope, window_label FROM provider_account_limit_observations \
             WHERE provider = ?1 AND source = ?2 AND observed_at < ?4 AND account = ?3",
    delete: "DELETE FROM provider_account_limit_observations \
             WHERE provider = ?1 AND model_scope = ?2 AND window_label = ?3 \
             AND source = ?4 AND observed_at < ?5 AND account = ?6",
    newer_exists: "SELECT EXISTS (SELECT 1 FROM provider_account_limit_observations \
             WHERE provider = ?1 AND source = ?2 AND observed_at > ?4 AND account = ?3)",
};

const DEFAULT_LOGIN_READING_SQL: ReadingSql = ReadingSql {
    select: "SELECT model_scope, window_label FROM provider_limit_observations \
             WHERE provider = ?1 AND source = ?2 AND observed_at < ?4",
    delete: "DELETE FROM provider_limit_observations \
             WHERE provider = ?1 AND model_scope = ?2 AND window_label = ?3 \
             AND source = ?4 AND observed_at < ?5",
    newer_exists: "SELECT EXISTS (SELECT 1 FROM provider_limit_observations \
             WHERE provider = ?1 AND source = ?2 AND observed_at > ?4)",
};

/// Literal statements for one reading's table; placeholder numbering matches
/// the bound parameters in [`ProviderLimitRepository::put_reading`].
struct ReadingSql {
    select: &'static str,
    delete: &'static str,
    newer_exists: &'static str,
}

/// Concrete provider-limit repository scoped to a [`Snapshot`] (read-only) or
/// a [`Tx`] (read-write).
pub struct ProviderLimitRepository<'a, Access> {
    conn: &'a Connection,
    _access: PhantomData<&'a Access>,
}

impl<'a, 's> ProviderLimitRepository<'a, Snapshot<'s>> {
    pub(crate) fn for_snapshot(snapshot: &'a Snapshot<'s>) -> Self {
        Self {
            conn: snapshot.connection(),
            _access: PhantomData,
        }
    }
}

impl<'a, 'tx> ProviderLimitRepository<'a, Tx<'tx>> {
    pub(crate) fn for_tx(tx: &'a Tx<'tx>) -> Self {
        Self {
            conn: tx.connection(),
            _access: PhantomData,
        }
    }

    /// Store `observation` as the latest for its provider, account, model
    /// scope and window, unless a newer one is stored. Returns whether it was
    /// stored.
    pub fn put(&self, observation: &ProviderLimitObservation) -> Result<bool, OrbitError> {
        upsert_observation(self.conn, observation)
    }

    /// [ORB-25967] Replace one provider account's `event` observations with a
    /// complete usage reading taken at one instant: store each observation as
    /// [`Self::put`] does, and remove the account's `event` rows of that
    /// provider that an older reading stored and this one no longer reports.
    /// `error` observations keep their own expiry. An empty reading changes
    /// nothing, and neither does a reading older than an `event` row the
    /// account already has: a retired key keeps no timestamp to reject the
    /// older insert, so writing it would restore a window the newer reading
    /// dropped.
    pub fn put_reading(&self, observations: &[ProviderLimitObservation]) -> Result<(), OrbitError> {
        let Some(first) = observations.first() else {
            return Ok(());
        };
        let account = account_of(first);
        // The statements are literals; the account is a crew name that only
        // ever travels as a bound parameter.
        let sql = if account.is_some() {
            &NAMED_CREW_READING_SQL
        } else {
            &DEFAULT_LOGIN_READING_SQL
        };
        let event = ProviderLimitSource::Event.as_str();
        let cutoff = stamp(first.observed_at);
        let account_text = account.unwrap_or_default();
        let superseded: bool = self
            .conn
            .query_row(
                sql.newer_exists,
                params![first.provider, event, account_text, cutoff],
                |row| row.get(0),
            )
            .map_err(sqlite_store_error)?;
        if superseded {
            return Ok(());
        }
        for observation in observations {
            upsert_observation(self.conn, observation)?;
        }
        let mut statement = self.conn.prepare(sql.select).map_err(sqlite_store_error)?;
        // `?3` is unused without an account; the placeholder keeps the
        // numbered parameters contiguous.
        let bound: [&dyn rusqlite::ToSql; 4] = [&first.provider, &event, &account_text, &cutoff];
        let stale = statement
            .query_map(bound, |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(sqlite_store_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_store_error)?;
        drop(statement);
        for (model, window) in stale {
            let reported = observations.iter().any(|observation| {
                observation.model.as_deref().unwrap_or_default() == model
                    && observation.window.as_deref().unwrap_or_default() == window
            });
            if reported {
                continue;
            }
            let mut bound: Vec<&dyn rusqlite::ToSql> =
                vec![&first.provider, &model, &window, &event, &cutoff];
            if account.is_some() {
                bound.push(&account_text);
            }
            self.conn
                .execute(sql.delete, bound.as_slice())
                .map_err(sqlite_store_error)?;
        }
        Ok(())
    }

    /// [ORB-16379] Store `outage` for its provider and crew, unless the stored
    /// one lasts at least as long. Returns whether it was stored.
    pub fn put_outage(&self, outage: &ProviderCrewOutage) -> Result<bool, OrbitError> {
        self.conn
            .execute(
                r#"
                INSERT INTO provider_crew_outages (
                    provider, crew, until_at, observed_at, run_id
                ) VALUES (?1, ?2, ?3, ?4, ?5)
                ON CONFLICT (provider, crew) DO UPDATE SET
                    until_at = excluded.until_at,
                    observed_at = excluded.observed_at,
                    run_id = excluded.run_id
                WHERE excluded.until_at > provider_crew_outages.until_at
                "#,
                params![
                    outage.provider.as_deref().unwrap_or_default(),
                    outage.crew,
                    stamp(outage.until),
                    stamp(outage.observed_at),
                    outage.run_id,
                ],
            )
            .map(|changed| changed > 0)
            .map_err(sqlite_store_error)
    }
}

impl<Access> ProviderLimitRepository<'_, Access> {
    /// The latest observation per provider, account, model scope and window,
    /// newest first.
    pub fn list(&self) -> Result<Vec<ProviderLimitObservation>, OrbitError> {
        // [ORB-17288] Rows of the default login's table, every one recorded
        // before accounts were named included, carry no account.
        let mut statement = self
            .conn
            .prepare(
                r#"
                SELECT provider, model_scope, window_label, exhausted, source,
                       resets_at, observed_at, run_id, crew, detail,
                       used_percent, window_minutes, gating, '' AS account
                FROM provider_limit_observations
                UNION ALL
                SELECT provider, model_scope, window_label, exhausted, source,
                       resets_at, observed_at, run_id, crew, detail,
                       used_percent, window_minutes, gating, account
                FROM provider_account_limit_observations
                ORDER BY observed_at DESC, provider, account, model_scope, window_label
                "#,
            )
            .map_err(sqlite_store_error)?;
        let non_empty = |value: String| (!value.is_empty()).then_some(value);
        let rows = statement
            .query_map([], |row| {
                let source: String = row.get(4)?;
                Ok(ProviderLimitObservation {
                    provider: row.get(0)?,
                    account: non_empty(row.get(13)?),
                    model: non_empty(row.get(1)?),
                    window: non_empty(row.get(2)?),
                    exhausted: row.get(3)?,
                    source: ProviderLimitSource::parse(&source).ok_or_else(|| {
                        rusqlite::Error::FromSqlConversionFailure(
                            4,
                            rusqlite::types::Type::Text,
                            format!("unknown provider limit source `{source}`").into(),
                        )
                    })?,
                    resets_at: row
                        .get::<_, Option<String>>(5)?
                        .map(|at| crate::parse_timestamp(&at))
                        .transpose()?,
                    observed_at: crate::parse_timestamp(&row.get::<_, String>(6)?)?,
                    run_id: row.get(7)?,
                    crew: row.get(8)?,
                    detail: row.get(9)?,
                    used_percent: row.get(10)?,
                    window_minutes: row.get(11)?,
                    gating: row.get(12)?,
                    partial: false,
                })
            })
            .map_err(sqlite_store_error)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_store_error)
    }

    /// [ORB-16379] Every crew outage lasting past `now`, newest first.
    pub fn list_outages(&self, now: DateTime<Utc>) -> Result<Vec<ProviderCrewOutage>, OrbitError> {
        let mut statement = self
            .conn
            .prepare(
                r#"
                SELECT provider, crew, until_at, observed_at, run_id
                FROM provider_crew_outages
                WHERE until_at > ?1
                ORDER BY observed_at DESC, provider, crew
                "#,
            )
            .map_err(sqlite_store_error)?;
        let rows = statement
            .query_map([stamp(now)], |row| {
                let provider: String = row.get(0)?;
                Ok(ProviderCrewOutage {
                    provider: (!provider.is_empty()).then_some(provider),
                    crew: row.get(1)?,
                    until: crate::parse_timestamp(&row.get::<_, String>(2)?)?,
                    observed_at: crate::parse_timestamp(&row.get::<_, String>(3)?)?,
                    run_id: row.get(4)?,
                })
            })
            .map_err(sqlite_store_error)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_store_error)
    }
}

/// The crew account an observation belongs to, or `None` for the provider's
/// default login. [ORB-17288]
fn account_of(observation: &ProviderLimitObservation) -> Option<&str> {
    observation
        .account
        .as_deref()
        .map(str::trim)
        .filter(|account| !account.is_empty())
}

/// Store `observation` as the latest of its key unless a newer one stands.
fn upsert_observation(
    conn: &Connection,
    observation: &ProviderLimitObservation,
) -> Result<bool, OrbitError> {
    // [ORB-17288] A crew account's readings are kept in their own table,
    // so the default login's rows, and an older binary's writes of them,
    // keep their key.
    let account = account_of(observation);
    // Keep SQL structure literal and observation data in bound parameters.
    // The account is a crew name, never the credential that crew uses.
    let sql = if account.is_some() {
        r#"
        INSERT INTO provider_account_limit_observations (
            account, provider, model_scope, window_label, exhausted, source,
            resets_at, observed_at, run_id, crew, detail,
            used_percent, window_minutes, gating
        ) VALUES (?14, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
        ON CONFLICT (provider, account, model_scope, window_label) DO UPDATE SET
            exhausted = excluded.exhausted,
            source = excluded.source,
            resets_at = excluded.resets_at,
            observed_at = excluded.observed_at,
            run_id = excluded.run_id,
            crew = excluded.crew,
            detail = excluded.detail,
            used_percent = excluded.used_percent,
            window_minutes = excluded.window_minutes,
            gating = excluded.gating
        WHERE excluded.observed_at >= provider_account_limit_observations.observed_at
        "#
    } else {
        r#"
        INSERT INTO provider_limit_observations (
            provider, model_scope, window_label, exhausted, source,
            resets_at, observed_at, run_id, crew, detail,
            used_percent, window_minutes, gating
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
        ON CONFLICT (provider, model_scope, window_label) DO UPDATE SET
            exhausted = excluded.exhausted,
            source = excluded.source,
            resets_at = excluded.resets_at,
            observed_at = excluded.observed_at,
            run_id = excluded.run_id,
            crew = excluded.crew,
            detail = excluded.detail,
            used_percent = excluded.used_percent,
            window_minutes = excluded.window_minutes,
            gating = excluded.gating
        WHERE excluded.observed_at >= provider_limit_observations.observed_at
        "#
    };
    let model = observation.model.as_deref().unwrap_or_default();
    let window = observation.window.as_deref().unwrap_or_default();
    let source = observation.source.as_str();
    let resets_at = observation.resets_at.map(stamp);
    let observed_at = stamp(observation.observed_at);
    let mut bound: Vec<&dyn rusqlite::ToSql> = vec![
        &observation.provider,
        &model,
        &window,
        &observation.exhausted,
        &source,
        &resets_at,
        &observed_at,
        &observation.run_id,
        &observation.crew,
        &observation.detail,
        &observation.used_percent,
        &observation.window_minutes,
        &observation.gating,
    ];
    if let Some(account) = &account {
        bound.push(account);
    }
    conn.execute(sql, bound.as_slice())
        .map(|changed| changed > 0)
        .map_err(sqlite_store_error)
}
