use std::fs;
use std::path::Path;

use serde_json::json;

use orbit_common::OrbitError;

use crate::contracts::InvocationStoreBackend;

use orbit_common::fs::io::{atomic_write_text_volatile as write_atomic, with_exclusive_file_lock};

const TOKEN_SCOREBOARD_FILENAME: &str = "tokens.json";
const TOKEN_SCOREBOARD_WATERMARK_FILENAME: &str = "tokens.watermark";

/// Writes `tokens.json` from the invocation store.
///
/// Skips the four aggregate queries and the rewrite when `tokens.json` already
/// exists and a cheap insert-only watermark (`MAX(invocations.id)`) matches the
/// sidecar stamped on the last successful write. The sidecar is not part of the
/// scoreboard payload, so a refresh that does run keeps the existing
/// `tokens.json` schema.
///
/// The `known_limitations` payload documents how these totals relate to the
/// external supervisor worker run store: they are
/// not interchangeable denominators.
pub fn write_token_scoreboard(
    scoreboard_dir: &Path,
    store: &dyn InvocationStoreBackend,
) -> Result<(), OrbitError> {
    let path = scoreboard_dir.join(TOKEN_SCOREBOARD_FILENAME);
    with_exclusive_file_lock(&path, "token scoreboard", || {
        let watermark = store.invocation_scoreboard_watermark()?;
        if path.is_file()
            && watermark.is_some_and(|current| read_watermark(scoreboard_dir) == Some(current))
        {
            return Ok(());
        }

        let payload = json!({
            "generated_at": chrono::Utc::now().to_rfc3339(),
            "activities": store.list_activity_invocation_metrics()?,
            "agents": store.list_agent_invocation_metrics()?,
            "top_tasks": store.list_top_task_invocation_metrics(20)?,
            "tools": store.list_tool_invocation_metrics()?,
            "known_limitations": [
                "Subagent attribution folds into the parent invocation totals.",
                "cache_read_tokens are reported separately from input_tokens.",
                "Multi-task invocations are fully attributed to every tagged task.",
                "Legacy agent invocations without a resolved model are omitted from the activities and agents sections.",
                "Providers without structured usage metadata may emit zero traces.",
                "Claude CLI result documents repeat the same billed session totals on `usage` and `modelUsage`. Ingest now keeps the `usage` rollup (including the cache-creation TTL split) and does not add sibling `modelUsage`. Already-persisted invocation rows are not rewritten, so historical Claude cache_read is typically ~2x the CLI result.",
                "The supervisor worker run store (~/.local/share/supervisor/runs/*.json) and this scoreboard's `invocations` table are not one ledger: no shared run id, different populations (all supervisor CLIs vs pipeline activities), and worker top-level `usage` is often absent. A historical 10-21x per-run comparison mixed those sets and treated missing worker fields as 0. Do not mix the stores for cost-per-token or tokens-per-minute.",
                "Use `invocations` / this scoreboard for pipeline activity token accounting (post-fix rows only). Use a worker run's result.usage / result.modelUsage for that CLI process's billed totals. provider_cost_usd and the worker's total_cost_usd agree and are the monthly reconciliation figure."
            ]
        });

        fs::create_dir_all(scoreboard_dir).map_err(|e| OrbitError::Io(e.to_string()))?;
        let raw = serde_json::to_string_pretty(&payload)
            .map_err(|e| OrbitError::Store(format!("serialize tokens.json: {e}")))?;
        write_atomic(&path, &format!("{raw}\n")).map_err(OrbitError::from)?;
        if let Some(watermark) = watermark {
            write_watermark(scoreboard_dir, watermark)?;
        }
        Ok(())
    })
}

fn watermark_path(scoreboard_dir: &Path) -> std::path::PathBuf {
    scoreboard_dir.join(TOKEN_SCOREBOARD_WATERMARK_FILENAME)
}

fn read_watermark(scoreboard_dir: &Path) -> Option<u64> {
    fs::read_to_string(watermark_path(scoreboard_dir))
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn write_watermark(scoreboard_dir: &Path, watermark: u64) -> Result<(), OrbitError> {
    write_atomic(&watermark_path(scoreboard_dir), &format!("{watermark}\n"))
        .map_err(OrbitError::from)
}
