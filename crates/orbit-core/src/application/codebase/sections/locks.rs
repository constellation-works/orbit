use super::*;

use super::rollup::*;

#[derive(Default)]
pub(in crate::application::codebase) struct LockRow {
    /// The requesters refused a lock on the path: the numerator.
    pub(in crate::application::codebase) contenders: BTreeSet<String>,
    pub(in crate::application::codebase) requesters: BTreeSet<String>,
    pub(in crate::application::codebase) reservations: usize,
    pub(in crate::application::codebase) denials: usize,
    /// The tasks' wait episodes on the path with a denial in the window.
    pub(in crate::application::codebase) waits: Vec<WaitEpisode>,
}

pub(in crate::application::codebase) fn total_wait_ms(row: &LockRow) -> i64 {
    row.waits.iter().map(WaitEpisode::wait_ms).sum()
}

pub(in crate::application::codebase) struct LockRows {
    pub(in crate::application::codebase) rows: BTreeMap<String, LockRow>,
    reservations: usize,
    denials: usize,
}

/// Distinct requesters that waited on a path because of a lock conflict ÷
/// distinct requesters that asked for it (reserved it or were refused it). A retry
/// loop repeats the same contender and counts once. Reservation and denial
/// counts are reported beside, not folded into, the contention figure, per
/// path and as window totals. Each task's wait episodes on the path are kept
/// beside them.
pub(in crate::application::codebase) fn lock_rows(
    facts: &CodebaseFacts,
    params: &Params,
) -> LockRows {
    let mut rows: BTreeMap<String, LockRow> = BTreeMap::new();
    let mut reservations = 0;
    let mut denials = 0;
    for reservation in &facts.reservations {
        if !params.within(reservation.at) {
            continue;
        }
        reservations += 1;
        for key in params.rollup.keys(&reservation.files) {
            let row = rows.entry(key).or_default();
            row.reservations += 1;
            row.requesters
                .extend(reservation.requesters.iter().cloned());
        }
    }
    for denial in &facts.lock_denials {
        if !params.within(denial.at) {
            continue;
        }
        denials += 1;
        for key in params.rollup.keys(&denial.files) {
            let row = rows.entry(key).or_default();
            row.denials += 1;
            row.contenders.extend(denial.contenders.iter().cloned());
            row.requesters.extend(denial.contenders.iter().cloned());
        }
    }
    for (key, wait) in wait_episodes(facts, params) {
        rows.entry(key).or_default().waits.push(wait);
    }
    LockRows {
        rows,
        reservations,
        denials,
    }
}

pub(in crate::application::codebase) fn lock_tally(row: &LockRow) -> (usize, usize) {
    (row.contenders.len(), row.requesters.len())
}

pub(in crate::application::codebase) fn lock_contention(
    facts: &CodebaseFacts,
    params: &Params,
) -> Value {
    let LockRows {
        rows,
        reservations,
        denials,
    } = lock_rows(facts, params);
    let (ranked, below_support) = rank(rows, params.min_support, lock_tally);
    let row_json = |path: &str, row: &LockRow| {
        merge(
            rate_fields(row.contenders.len(), row.requesters.len()),
            json!({
                "path": path,
                "contending_tasks": row.contenders.len(),
                "reservations": row.reservations,
                "denials": row.denials,
                "wait": wait_summary(&row.waits),
            }),
        )
    };
    let rows: Vec<Value> = ranked
        .iter()
        .take(ROW_LIMIT)
        .map(|(path, row)| row_json(path, row))
        .collect();
    // The same supported rows, ranked by how long tasks waited on them, so a
    // path that costs hours behind few denials is not cut by the rate order.
    let mut by_wait: Vec<&(String, LockRow)> = ranked
        .iter()
        .filter(|(_, row)| !row.waits.is_empty())
        .collect();
    by_wait.sort_by(|(left_path, left), (right_path, right)| {
        total_wait_ms(right)
            .cmp(&total_wait_ms(left))
            .then_with(|| left_path.cmp(right_path))
    });
    let rows_by_wait: Vec<Value> = by_wait
        .into_iter()
        .take(ROW_LIMIT)
        .map(|(path, row)| row_json(path, row))
        .collect();
    json!({
        "rows": rows,
        "rows_by_wait": rows_by_wait,
        "below_support": below_support,
        "reservations": reservations,
        "denials": denials,
        "wait_time_recorded": true,
    })
}
