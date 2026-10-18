//! One path's standing in every ranking, with what stands behind each
//! numerator: the tasks for the four task rankings, the refused requesters
//! for lock contention and the reserving requesters of each coupled pair.
//!
//! It reads the rows the rankings rank, so a count here is the count the
//! ranking shows, and reports the path's place in each ranking's whole order
//! (not only its listed rows), or `null` when the path is not ranked there.

use chrono::{DateTime, SecondsFormat, Utc};
use orbit_common::OrbitError;
use serde_json::{Value, json};

use super::facts::{CodebaseFacts, TaskFacts};
use super::sections::{
    Params, blocked_rows, coupling_pairs, footprint_rows, lock_rows, lock_tally, merge, rank,
    rank_blocked, rate_fields, regression_rows, rejected_rows,
};

pub(super) fn path_detail(
    facts: &CodebaseFacts,
    params: &Params,
    path: &str,
) -> Result<Value, OrbitError> {
    let (regressions, _) = regression_rows(facts, params);
    let (footprint, _) = footprint_rows(facts, params);
    let blocked = blocked_rows(facts, params).rows;
    let rejected = rejected_rows(facts, params).rows;
    let lock = lock_rows(facts, params).rows;
    let coupling = coupling_pairs(facts, params);

    // A directory or crate key ends in `/`; accept it with or without.
    let requested = path.trim();
    let requested = requested
        .strip_prefix("./")
        .filter(|rest| !rest.is_empty())
        .unwrap_or(requested);
    let alternate = match requested.strip_suffix('/') {
        Some(bare) => bare.to_string(),
        None => format!("{requested}/"),
    };
    let known = |key: &str| {
        regressions.contains_key(key)
            || footprint.contains_key(key)
            || blocked.contains_key(key)
            || rejected.contains_key(key)
            || lock.contains_key(key)
            || coupling.singles.contains_key(key)
    };
    let Some(key) = [requested.to_string(), alternate]
        .into_iter()
        .find(|key| !key.is_empty() && known(key))
    else {
        return Err(OrbitError::InvalidInput(format!(
            "no task or reservation in this window touched '{requested}' at the {} rollup",
            params.rollup.as_str()
        )));
    };
    let min_support = params.min_support;

    let regression = regressions.get(&key).map(|row| {
        let mut tasks = row.regressed.clone();
        tasks.sort_by(|left, right| {
            right
                .done_at
                .cmp(&left.done_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        let tasks: Vec<Value> = tasks
            .into_iter()
            .map(|task| {
                let mut entry = task_entry(task, task.done_at.unwrap_or(task.updated_at));
                entry["crew"] = json!(task.crew);
                entry["regression_tasks"] = json!(task.regressed_by);
                entry
            })
            .collect();
        merge(
            rate_fields(row.tally.numerator, row.tally.denominator),
            json!({ "tasks": tasks }),
        )
    });
    let (ranked, _) = rank(regressions, min_support, |row| {
        (row.tally.numerator, row.tally.denominator)
    });
    let regression = standing(&key, regression, &ranked, min_support);

    let footprint_row = footprint.get(&key).map(|row| {
        merge(
            rate_fields(row.tally.numerator, row.tally.denominator),
            json!({ "widenings": row.widenings, "tasks": task_entries(&row.missed) }),
        )
    });
    let (ranked, _) = rank(footprint, min_support, |row| {
        (row.tally.numerator, row.tally.denominator)
    });
    let footprint_row = standing(&key, footprint_row, &ranked, min_support);

    let blocked_row = blocked.get(&key).map(|row| {
        merge(
            rate_fields(row.tally.numerator, row.tally.denominator),
            json!({
                "tasks": task_entries(&row.blocked),
                "unclassified": row.unclassified.len(),
                "unclassified_tasks": task_entries(&row.unclassified),
            }),
        )
    });
    let (ranked, _) = rank_blocked(blocked, min_support);
    let blocked_row = standing(&key, blocked_row, &ranked, min_support);

    let rejected_row = rejected.get(&key).map(|row| {
        merge(
            rate_fields(row.tally.numerator, row.tally.denominator),
            json!({ "tasks": task_entries(&row.rejected) }),
        )
    });
    let (ranked, _) = rank(rejected, min_support, |row| {
        (row.tally.numerator, row.tally.denominator)
    });
    let rejected_row = standing(&key, rejected_row, &ranked, min_support);

    let lock_row = lock.get(&key).map(|row| {
        let (numerator, denominator) = lock_tally(row);
        merge(
            rate_fields(numerator, denominator),
            json!({
                "contenders": row.contenders,
                "reservations": row.reservations,
                "denials": row.denials,
            }),
        )
    });
    let (ranked, _) = rank(lock, min_support, lock_tally);
    let lock_row = standing(&key, lock_row, &ranked, min_support);

    let pairs: Vec<Value> = coupling
        .ranked
        .iter()
        .enumerate()
        .filter(|(_, pair)| pair.paths.0 == key || pair.paths.1 == key)
        .map(|(index, pair)| {
            let requesters: std::collections::BTreeSet<&str> = pair
                .both
                .iter()
                .flat_map(|reservation| reservation.requesters.iter().map(String::as_str))
                .collect();
            merge(
                rate_fields(pair.both.len(), pair.either),
                json!({
                    "paths": [pair.paths.0, pair.paths.1],
                    "rank": index + 1,
                    "reservations": [pair.counts.0, pair.counts.1],
                    "requesters": requesters,
                }),
            )
        })
        .collect();
    let coupling_row = json!({
        "reservations": coupling.singles.get(&key).copied().unwrap_or(0),
        "pairs": pairs,
    });

    Ok(json!({
        "path": key,
        "rankings": {
            "regressions": regression,
            "footprint_misses": footprint_row,
            "blocked": blocked_row,
            "rejected": rejected_row,
            "lock_contention": lock_row,
            "change_coupling": coupling_row,
        },
    }))
}

/// A ranking's row for the path with its 1-based place in the ranking's
/// whole order (`null` when unranked) and whether its denominator falls
/// below the minimum support; `null` when the ranking has no row for it.
fn standing<T>(key: &str, row: Option<Value>, ranked: &[(String, T)], min_support: usize) -> Value {
    let Some(row) = row else {
        return Value::Null;
    };
    let rank = ranked
        .iter()
        .position(|(path, _)| path == key)
        .map(|index| index + 1);
    let below_support = row["denominator"]
        .as_u64()
        .is_some_and(|denominator| denominator < min_support as u64);
    merge(row, json!({ "rank": rank, "below_support": below_support }))
}

/// Tasks newest first, each with the time that put it in the numerator.
fn task_entries(tasks: &[(&TaskFacts, DateTime<Utc>)]) -> Vec<Value> {
    let mut tasks = tasks.to_vec();
    tasks.sort_by(|(left, left_at), (right, right_at)| {
        right_at.cmp(left_at).then_with(|| left.id.cmp(&right.id))
    });
    tasks
        .into_iter()
        .map(|(task, at)| task_entry(task, at))
        .collect()
}

fn task_entry(task: &TaskFacts, at: DateTime<Utc>) -> Value {
    json!({
        "task_id": task.id,
        "title": task.title,
        "at": at.to_rfc3339_opts(SecondsFormat::Secs, true),
    })
}
