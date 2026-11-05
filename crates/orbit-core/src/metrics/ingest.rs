use orbit_types::telemetry::InvocationTrace;
use orbit_types::workflow::KnowledgeRunMetrics;

use super::summary::ratio;

/// Fold a new invocation into persisted [`KnowledgeRunMetrics`].
///
/// A trace alone never creates a row: without existing metrics there is
/// nothing to merge into, so this returns `None`. For an existing row the
/// merge accumulates the trace's LLM input tokens; stored
/// `actual_fs_read_tokens_during_run` / `double_read_rate` values are left
/// untouched.
pub(crate) fn merge_invocation_trace(
    existing: Option<&KnowledgeRunMetrics>,
    trace: &InvocationTrace,
) -> Option<KnowledgeRunMetrics> {
    let mut metrics = existing.cloned()?;
    metrics.total_llm_input_tokens = metrics.total_llm_input_tokens.saturating_add(
        trace
            .usage
            .input
            .saturating_add(trace.usage.cache_read)
            .saturating_add(trace.usage.cache_create)
            .saturating_add(trace.usage.cache_create_1h),
    );

    metrics.compression_ratio = metrics
        .knowledge_pack_used
        .then(|| {
            ratio(
                metrics.raw_read_token_baseline,
                metrics.knowledge_pack_tokens.unwrap_or(0),
            )
        })
        .flatten();

    Some(metrics)
}
