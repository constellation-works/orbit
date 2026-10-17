//! [ORB-14695] The host's provider usage limits in `provider_limit_observations`,
//! and [ORB-16379] its crew outages in `provider_crew_outages`.
//!
//! Timestamps are stored as fixed-width UTC RFC3339 text (microseconds, `Z`),
//! so SQLite orders them by comparing the text.

use chrono::{DateTime, SecondsFormat, Utc};
use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use orbit_types::telemetry::{ProviderLimitObservation, ProviderLimitSource};
use orbit_types::workflow::ProviderCrewOutage;
use rusqlite::{TransactionBehavior, params};

use crate::Store;
use crate::contracts::ProviderLimitStoreBackend;

fn stamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Micros, true)
}

impl ProviderLimitStoreBackend for Store {
    fn record_provider_limit(
        &self,
        observation: &ProviderLimitObservation,
    ) -> Result<bool, OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            tx.connection()
                .execute(
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
                    "#,
                    params![
                        observation.provider,
                        observation.model.as_deref().unwrap_or_default(),
                        observation.window.as_deref().unwrap_or_default(),
                        observation.exhausted,
                        observation.source.as_str(),
                        observation.resets_at.map(stamp),
                        stamp(observation.observed_at),
                        observation.run_id,
                        observation.crew,
                        observation.detail,
                        observation.used_percent,
                        observation.window_minutes,
                        observation.gating,
                    ],
                )
                .map(|changed| changed > 0)
                .map_err(sqlite_store_error)
        })
    }

    fn provider_limits(&self) -> Result<Vec<ProviderLimitObservation>, OrbitError> {
        let conn = self.read()?;
        let mut statement = conn
            .prepare(
                r#"
                SELECT provider, model_scope, window_label, exhausted, source,
                       resets_at, observed_at, run_id, crew, detail,
                       used_percent, window_minutes, gating
                FROM provider_limit_observations
                ORDER BY observed_at DESC, provider, model_scope, window_label
                "#,
            )
            .map_err(sqlite_store_error)?;
        let non_empty = |value: String| (!value.is_empty()).then_some(value);
        let rows = statement
            .query_map([], |row| {
                let source: String = row.get(4)?;
                Ok(ProviderLimitObservation {
                    provider: row.get(0)?,
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

    fn record_provider_crew_outage(&self, outage: &ProviderCrewOutage) -> Result<bool, OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            tx.connection()
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
        })
    }

    fn provider_crew_outages(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Vec<ProviderCrewOutage>, OrbitError> {
        let conn = self.read()?;
        let mut statement = conn
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
