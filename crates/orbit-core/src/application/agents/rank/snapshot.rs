//! Frozen ranking snapshots (design §6): the settled ranking for one
//! cutoff, written once with every input it was computed from, so the
//! result can be recomputed from the snapshot alone and checked against
//! what was frozen. A snapshot is never overwritten: freezing a cutoff
//! that already has one returns the stored snapshot.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use orbit_common::OrbitError;
use orbit_common::fs::io::{create_private_dir_all, open_read_only_no_follow};
use orbit_common::security::release::sha256_hex;
use serde_json::{Value, json};

use super::board::{self, ModelRow};
use super::measures::{MEASURE_COUNT, MEASURES, Observations};
use super::sample::{BOOTSTRAP_SEED, CALIBRATION_SEED, Record};
use super::{RankQuery, RankSeasons, agent_rank, board_inputs};
use crate::OrbitRuntime;
use crate::application::agents::{AgentFacts, MODEL_UNKNOWN};

/// The snapshot document's schema version.
pub(crate) const RANK_SNAPSHOT_SCHEMA: u32 = 1;

const SNAPSHOTS_DIR: &str = "agents/rank-snapshots";

fn invalid(message: impl Into<String>) -> OrbitError {
    OrbitError::InvalidInput(message.into())
}

fn store(message: impl Into<String>) -> OrbitError {
    OrbitError::Store(message.into())
}

/// The settled cutoff a snapshot names: the ranked window's end.
fn snapshot_cutoff(ranking: &Value) -> Result<String, OrbitError> {
    ranking["until"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| store("ranking payload carries no cutoff"))
}

/// Build the snapshot for `query` without writing it. A live board is
/// provisional and never frozen, and an unqualified board (fewer than
/// three eligible models) is never frozen as a result. The snapshot is
/// verified against a recompute from its own inputs before it returns.
pub fn agent_rank_freeze_snapshot(
    facts: &AgentFacts,
    query: &RankQuery,
    seasons: &RankSeasons,
    frozen_at: DateTime<Utc>,
) -> Result<Value, OrbitError> {
    if query.live {
        return Err(invalid(
            "a live ranking is provisional and is never frozen; drop live",
        ));
    }
    let ranking = agent_rank(facts, query, seasons, Duration::ZERO);
    if ranking["qualified"] != Value::Bool(true) {
        return Err(invalid(format!(
            "the board at {} is not qualified ({} eligible models of 3 needed) and is never frozen as a result; widen the window or move until",
            ranking["until"].as_str().unwrap_or("the settled cutoff"),
            ranking["eligible_models"].as_u64().unwrap_or(0),
        )));
    }
    let cutoff = snapshot_cutoff(&ranking)?;
    let (_window, rows, _scales) = board_inputs(facts, query, seasons);

    let mut inputs_rows = Vec::with_capacity(rows.len());
    for row in &rows {
        let measures = ranking["rows"]
            .as_array()
            .and_then(|ranked| ranked.iter().find(|ranked| ranked["model"] == row.model))
            .map(|ranked| ranked["measures"].clone())
            .unwrap_or_else(|| json!({}));
        let records: Vec<Value> = row
            .records
            .iter()
            .zip(&row.costs)
            .map(|(record, (cost_tier, cost))| {
                json!({
                    "tier": record.tier,
                    "start": record.start.to_rfc3339_opts(SecondsFormat::Secs, true),
                    "cost_tier": cost_tier,
                    "cost": cost,
                    "observed": record.observed.iter().map(|observation| match observation {
                        Some((actual, expected)) => json!([actual, expected]),
                        None => Value::Null,
                    }).collect::<Vec<_>>(),
                })
            })
            .collect();
        inputs_rows.push(json!({
            "model": row.model,
            "unattributed": row.model == MODEL_UNKNOWN,
            "crews": row.crews.iter().map(|(crew, tasks)| json!({"crew": crew, "tasks": tasks})).collect::<Vec<_>>(),
            "units": row.units(),
            "landed": row.landed,
            "lines": {"changed": row.lines, "tasks": row.sized_tasks},
            "measures": measures,
            "records": records,
        }));
    }

    let mut rules = ranking["rules"].clone();
    rules["bootstrap_seed"] = json!(BOOTSTRAP_SEED);
    rules["calibration_seed"] = json!(CALIBRATION_SEED);

    let inputs = json!({
        "window": ranking["window"],
        "requested_window": ranking["requested_window"],
        "since": ranking["since"],
        "cutoff": cutoff,
        "requested_until": ranking["requested_until"],
        "settle_lag": ranking["settle_lag"],
        "execution": ranking["execution"],
        "definition": ranking["definition"],
        "scales_source": ranking["scales_source"],
        "season": ranking["season"],
        "measures": ranking["measures"],
        "weights_total": ranking["weights_total"],
        "counting_order": ranking["counting_order"],
        "rules": rules,
        "rows": inputs_rows,
    });

    let mut frozen_ranking = ranking;
    if let Some(fields) = frozen_ranking.as_object_mut() {
        fields.remove("computed_at");
        fields.remove("cache_ttl_seconds");
        fields.remove("tasks_scanned");
    }

    let mut snapshot = json!({
        "schema_version": RANK_SNAPSHOT_SCHEMA,
        "kind": "model_ranking_snapshot",
        "cutoff": cutoff,
        "frozen_at": frozen_at.to_rfc3339_opts(SecondsFormat::Secs, true),
        "inputs": inputs,
        "ranking": frozen_ranking,
    });
    snapshot["content_hash"] = json!(rank_snapshot_content_hash(&snapshot)?);

    let report = rank_snapshot_recompute(&snapshot)?;
    if report["recomputes"] != Value::Bool(true) {
        return Err(OrbitError::Execution(format!(
            "frozen snapshot does not recompute from its inputs: {}",
            report["mismatches"]
        )));
    }
    Ok(snapshot)
}

/// The `sha256` of the snapshot's canonical content: schema, cutoff,
/// inputs and ranking with object keys sorted recursively. `frozen_at`
/// and the hash itself are outside the hashed content, so refreezing the
/// same facts at a different time hashes the same.
pub fn rank_snapshot_content_hash(snapshot: &Value) -> Result<String, OrbitError> {
    let content = json!({
        "schema_version": snapshot["schema_version"],
        "kind": snapshot["kind"],
        "cutoff": snapshot["cutoff"],
        "inputs": snapshot["inputs"],
        "ranking": snapshot["ranking"],
    });
    let mut text = String::new();
    canonical_text(&content, &mut text);
    Ok(sha256_hex(text.as_bytes()))
}

/// Canonical text for hashing: object keys sorted, floats at a fixed
/// precision. A stored snapshot parses its floats back through
/// `serde_json`, whose fast parser can land one ulp from the value that
/// was written; hashing the raw float text would then never verify.
/// Eleven significant digits are far beyond every rounded figure the
/// ranking publishes and immune to that last-ulp wobble.
fn canonical_text(value: &Value, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
        Value::Number(number) => match number.as_f64() {
            Some(float) if number.is_f64() => {
                out.push_str(&format!("{:.10e}", float + 0.0));
            }
            _ => out.push_str(&number.to_string()),
        },
        Value::String(text) => {
            out.push_str(&serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_string()));
        }
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                canonical_text(item, out);
            }
            out.push(']');
        }
        Value::Object(fields) => {
            let mut sorted: Vec<_> = fields.iter().collect();
            sorted.sort_by_key(|(key, _)| *key);
            out.push('{');
            for (index, (key, value)) in sorted.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(key).unwrap_or_else(|_| "\"\"".to_string()));
                out.push(':');
                canonical_text(value, out);
            }
            out.push('}');
        }
    }
}

fn tier_static(tier: &str) -> Option<&'static str> {
    match tier {
        "low" => Some("low"),
        "medium" => Some("medium"),
        "hard" => Some("hard"),
        "xhard" => Some("xhard"),
        "unassessed" => Some("unassessed"),
        _ => None,
    }
}

fn rows_from_inputs(snapshot: &Value) -> Result<Vec<ModelRow>, OrbitError> {
    let inputs_rows = snapshot["inputs"]["rows"]
        .as_array()
        .ok_or_else(|| invalid("ranking snapshot inputs carry no rows"))?;
    let mut rows = Vec::with_capacity(inputs_rows.len());
    for input in inputs_rows {
        let model = input["model"]
            .as_str()
            .ok_or_else(|| invalid("ranking snapshot input row carries no model"))?
            .to_string();
        let mut crews = BTreeMap::new();
        for crew in input["crews"].as_array().into_iter().flatten() {
            if let (Some(name), Some(tasks)) = (crew["crew"].as_str(), crew["tasks"].as_u64()) {
                crews.insert(name.to_string(), tasks as usize);
            }
        }
        let mut records = Vec::new();
        let mut costs = Vec::new();
        for record in input["records"].as_array().into_iter().flatten() {
            let tier = record["tier"]
                .as_str()
                .and_then(tier_static)
                .ok_or_else(|| {
                    invalid(format!(
                        "ranking snapshot record has an unknown tier: {}",
                        record["tier"]
                    ))
                })?;
            let cost_tier = record["cost_tier"]
                .as_str()
                .and_then(tier_static)
                .unwrap_or(tier);
            let start = record["start"]
                .as_str()
                .and_then(|start| DateTime::parse_from_rfc3339(start).ok())
                .map(|start| start.with_timezone(&Utc))
                .ok_or_else(|| invalid("ranking snapshot record has no valid start"))?;
            let observed_values = record["observed"]
                .as_array()
                .ok_or_else(|| invalid("ranking snapshot record carries no observations"))?;
            if observed_values.len() != MEASURE_COUNT {
                return Err(invalid(format!(
                    "ranking snapshot record carries {} observations, expected {MEASURE_COUNT}",
                    observed_values.len()
                )));
            }
            let mut observed: Observations = [None; MEASURE_COUNT];
            for (index, value) in observed_values.iter().enumerate() {
                if let Some(pair) = value.as_array() {
                    let actual = pair
                        .first()
                        .and_then(Value::as_f64)
                        .ok_or_else(|| invalid("ranking snapshot observation has no actual"))?;
                    let expected = pair
                        .get(1)
                        .and_then(Value::as_f64)
                        .ok_or_else(|| invalid("ranking snapshot observation has no expected"))?;
                    observed[index] = Some((actual, expected));
                }
            }
            costs.push((cost_tier, record["cost"].as_f64()));
            records.push(Record {
                tier,
                start,
                observed,
            });
        }
        rows.push(ModelRow {
            model,
            crews,
            records,
            costs,
            landed: input["landed"].as_f64().unwrap_or(0.0),
            lines: input["lines"]["changed"].as_u64().unwrap_or(0),
            sized_tasks: input["lines"]["tasks"].as_u64().unwrap_or(0) as usize,
        });
    }
    Ok(rows)
}

fn scales_from_inputs(snapshot: &Value) -> Result<[Option<f64>; MEASURE_COUNT], OrbitError> {
    let measures = snapshot["inputs"]["measures"]
        .as_array()
        .ok_or_else(|| invalid("ranking snapshot inputs carry no measures"))?;
    let mut scales = [None; MEASURE_COUNT];
    for (index, measure) in MEASURES.iter().enumerate() {
        let input = measures
            .iter()
            .find(|input| input["key"] == measure.key)
            .ok_or_else(|| {
                invalid(format!(
                    "ranking snapshot inputs carry no {} measure",
                    measure.key
                ))
            })?;
        scales[index] = input["scale"].as_f64();
    }
    Ok(scales)
}

fn close(stored: &Value, recomputed: Option<f64>, tolerance: f64) -> bool {
    match (stored.as_f64(), recomputed) {
        (Some(stored), Some(recomputed)) => (stored - recomputed).abs() <= tolerance,
        (None, None) => stored.is_null(),
        _ => false,
    }
}

/// Recompute the board from the snapshot's own inputs (per-task records,
/// season scales, weights and the fixed bootstrap seed) and compare it
/// with the frozen ranking. Returns `{"recomputes": bool, "mismatches":
/// [...]}`; a malformed snapshot is an error, a diverging one is not.
pub fn rank_snapshot_recompute(snapshot: &Value) -> Result<Value, OrbitError> {
    if snapshot["schema_version"].as_u64() != Some(u64::from(RANK_SNAPSHOT_SCHEMA)) {
        return Err(invalid(format!(
            "ranking snapshot schema_version must be {RANK_SNAPSHOT_SCHEMA}"
        )));
    }
    let rows = rows_from_inputs(snapshot)?;
    let scales = scales_from_inputs(snapshot)?;
    let board = board::compute(&rows, &scales);
    let ranking = &snapshot["ranking"];
    let mut mismatches: Vec<String> = Vec::new();
    let note = |mismatches: &mut Vec<String>, message: String| {
        if mismatches.len() < 20 {
            mismatches.push(message);
        }
    };
    if ranking["qualified"].as_bool() != Some(board.qualified) {
        note(&mut mismatches, "qualified differs".to_string());
    }
    for ranked in ranking["rows"].as_array().into_iter().flatten() {
        let Some(model) = ranked["model"].as_str() else {
            continue;
        };
        let Some(index) = rows.iter().position(|row| row.model == model) else {
            note(&mut mismatches, format!("{model}: no input row"));
            continue;
        };
        let placed = &board.placed[index];
        if !close(&ranked["score"], placed.score(), 0.011) {
            note(
                &mut mismatches,
                format!(
                    "{model}: score {} vs recomputed {:?}",
                    ranked["score"],
                    placed.score()
                ),
            );
        }
        if !close(&ranked["z"], placed.z, 0.0011) {
            note(&mut mismatches, format!("{model}: z differs"));
        }
        if ranked["rank"].as_u64() != placed.band.map(|band| band as u64) {
            note(
                &mut mismatches,
                format!(
                    "{model}: rank {} vs recomputed {:?}",
                    ranked["rank"], placed.band
                ),
            );
        }
        let interval = &ranked["interval"];
        match (interval.is_object(), placed.interval) {
            (true, Some((low, high))) => {
                if !close(&interval["p10"], Some(low), 0.011)
                    || !close(&interval["p90"], Some(high), 0.011)
                {
                    note(&mut mismatches, format!("{model}: interval differs"));
                }
            }
            (false, None) => {}
            _ => note(
                &mut mismatches,
                format!("{model}: interval presence differs"),
            ),
        }
        if !close(&ranked["cost"]["ratio"], placed.cost.ratio, 0.0011) {
            note(&mut mismatches, format!("{model}: cost ratio differs"));
        }
    }
    if let Some(paired) = ranking["paired_probabilities"].as_object() {
        for (a, others) in paired {
            for (b, stored) in others.as_object().into_iter().flatten() {
                let pair = rows
                    .iter()
                    .position(|row| &row.model == a)
                    .zip(rows.iter().position(|row| &row.model == b));
                let recomputed = pair.and_then(|pair| board.paired.get(&pair).copied());
                if !close(stored, recomputed, 0.011) {
                    note(
                        &mut mismatches,
                        format!("paired probability {a}>{b} differs"),
                    );
                }
            }
        }
    }
    Ok(json!({
        "recomputes": mismatches.is_empty(),
        "mismatches": mismatches,
    }))
}

/// Whether the snapshot's stored hash matches its content.
pub fn rank_snapshot_hash_valid(snapshot: &Value) -> bool {
    snapshot["content_hash"]
        .as_str()
        .is_some_and(|stored| rank_snapshot_content_hash(snapshot).is_ok_and(|hash| hash == stored))
}

/// The file a cutoff's snapshot lives in, by its sanitized instant.
fn snapshot_file_name(cutoff: &str) -> String {
    format!("{}.json", cutoff.replace(':', "-"))
}

fn snapshot_id(cutoff: &str) -> String {
    cutoff.replace(':', "-")
}

/// Write `snapshot` under `dir` if its cutoff has none yet. An existing
/// snapshot is returned as stored and never overwritten, even when the
/// new computation differs; a stored snapshot whose hash no longer
/// matches its content is an error rather than a silent replacement.
pub fn write_rank_snapshot_immutable(
    dir: &Path,
    snapshot: &Value,
) -> Result<(Value, bool), OrbitError> {
    let cutoff = snapshot["cutoff"]
        .as_str()
        .ok_or_else(|| store("ranking snapshot carries no cutoff"))?;
    create_private_dir_all(dir).map_err(|error| OrbitError::from_write_io(dir, error))?;
    let path = dir.join(snapshot_file_name(cutoff));
    let text = serde_json::to_string_pretty(snapshot)
        .map_err(|error| store(format!("serialize ranking snapshot: {error}")))?;
    // Write beside the destination, then hard-link it into place: the link
    // fails when the cutoff already has a snapshot, and no reader ever sees
    // a half-written snapshot at the destination.
    let staging = dir.join(format!(
        ".{}.{}.tmp",
        snapshot_id(cutoff),
        std::process::id()
    ));
    let staged = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staging)
        .and_then(|mut file| {
            file.write_all(text.as_bytes())
                .and_then(|()| file.write_all(b"\n"))
        });
    if let Err(error) = staged {
        let _ = std::fs::remove_file(&staging);
        return Err(OrbitError::from_write_io(&path, error));
    }
    let linked = std::fs::hard_link(&staging, &path);
    let _ = std::fs::remove_file(&staging);
    match linked {
        Ok(()) => Ok((snapshot.clone(), false)),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let stored = read_rank_snapshot_file(&path)?;
            Ok((stored, true))
        }
        Err(error) => Err(OrbitError::from_write_io(&path, error)),
    }
}

fn read_rank_snapshot_file(path: &Path) -> Result<Value, OrbitError> {
    let mut text = String::new();
    open_read_only_no_follow(path).and_then(|mut file| file.read_to_string(&mut text))?;
    let snapshot: Value = serde_json::from_str(&text).map_err(|error| {
        store(format!(
            "ranking snapshot at {} is unreadable: {error}",
            path.display()
        ))
    })?;
    if !rank_snapshot_hash_valid(&snapshot) {
        return Err(store(format!(
            "ranking snapshot at {} does not match its content hash; it is left as stored and never overwritten",
            path.display()
        )));
    }
    Ok(snapshot)
}

/// Resolve a stored snapshot under `dir`: the one at `cutoff` (a full
/// RFC 3339 instant, its sanitized form, or a `YYYY-MM-DD` date naming
/// that day's snapshot), or the newest when `cutoff` is omitted.
pub fn read_rank_snapshot(dir: &Path, cutoff: Option<&str>) -> Result<Value, OrbitError> {
    let mut files: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "json")
            })
            .collect(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    files.sort();
    let path = match cutoff {
        None => files.last().cloned(),
        Some(cutoff) => {
            let wanted = snapshot_file_name(cutoff.trim());
            let date_prefix = cutoff.trim().get(..10).unwrap_or_default();
            files
                .iter()
                .find(|path| path.file_name().is_some_and(|name| name == wanted.as_str()))
                .cloned()
                .or_else(|| {
                    let mut by_date: Vec<_> = files
                        .iter()
                        .filter(|path| {
                            path.file_name()
                                .and_then(|name| name.to_str())
                                .is_some_and(|name| name.starts_with(date_prefix))
                        })
                        .collect();
                    (by_date.len() == 1).then(|| by_date.remove(0).clone())
                })
        }
    };
    let Some(path) = path else {
        return Err(invalid(match cutoff {
            Some(cutoff) => format!(
                "no frozen ranking snapshot at cutoff '{cutoff}'; freeze one with orbit agents rank --freeze"
            ),
            None => "no frozen ranking snapshot yet; freeze one with orbit agents rank --freeze"
                .to_string(),
        }));
    };
    read_rank_snapshot_file(&path)
}

fn snapshots_dir(runtime: &OrbitRuntime) -> PathBuf {
    runtime.paths().state_dir.join(SNAPSHOTS_DIR)
}

/// The response a surface returns: the stored snapshot plus its id, hash
/// check and recompute check at read time.
fn snapshot_response(snapshot: Value, already_frozen: Option<bool>) -> Result<Value, OrbitError> {
    let report = rank_snapshot_recompute(&snapshot)?;
    let mut response = snapshot;
    let cutoff = response["cutoff"].as_str().unwrap_or_default().to_string();
    response["snapshot_id"] = json!(snapshot_id(&cutoff));
    response["hash_valid"] = json!(rank_snapshot_hash_valid(&response));
    response["recomputes"] = report["recomputes"].clone();
    response["recompute_mismatches"] = report["mismatches"].clone();
    if let Some(already_frozen) = already_frozen {
        response["already_frozen"] = json!(already_frozen);
    }
    Ok(response)
}

impl OrbitRuntime {
    /// Freeze the settled ranking for `query` as an immutable snapshot.
    /// Without an explicit `until`, the settled cutoff is aligned to the
    /// latest Monday 00:00 UTC at or before `until − settle_lag`, so a
    /// weekly freeze names one snapshot per week. Freezing a cutoff that
    /// already has a snapshot returns the stored one.
    pub fn agent_rank_freeze(&self, query: &RankQuery) -> Result<Value, OrbitError> {
        let query = query.weekly_aligned();
        let seasons = self.agent_rank_seasons()?;
        let snapshot =
            agent_rank_freeze_snapshot(&self.agent_scan()?, &query, &seasons, Utc::now())?;
        let (stored, already_frozen) =
            write_rank_snapshot_immutable(&snapshots_dir(self), &snapshot)?;
        snapshot_response(stored, Some(already_frozen))
    }

    /// A frozen ranking snapshot: at `cutoff`, or the newest when omitted,
    /// with its hash and recompute checked at read time.
    pub fn agent_rank_snapshot(&self, cutoff: Option<&str>) -> Result<Value, OrbitError> {
        let stored = read_rank_snapshot(&snapshots_dir(self), cutoff)?;
        snapshot_response(stored, None)
    }
}
