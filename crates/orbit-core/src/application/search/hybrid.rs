use std::collections::BTreeMap;

use orbit_common::OrbitError;
use orbit_search::{
    DocLexicalHit, DocSearchResult, DocSearchSource, DocSemanticHit, IndexedDocFields, VectorStore,
};

use super::convert::doc_result_to_global;
use super::types::GlobalSearchHit;
use super::{
    DOC_HYBRID_FALLBACK_NOTE, DOC_SEARCH_MIN_CANDIDATES, DOC_SEARCH_OVERFETCH,
    TASK_HYBRID_FALLBACK_NOTE,
};
use crate::application::docs::DocRecord;

pub(super) fn doc_search_candidate_limit(limit: usize) -> usize {
    limit
        .saturating_mul(DOC_SEARCH_OVERFETCH)
        .max(DOC_SEARCH_MIN_CANDIDATES)
}

/// The frontmatter a hybrid doc candidate is completed and tag-filtered
/// from, whichever corpus read produced it.
#[derive(Debug, Clone)]
pub(super) struct DocHybridRecord {
    pub(super) summary: String,
    /// `None` for a record read from the index, which stores no doc type.
    pub(super) doc_type: Option<String>,
    pub(super) tags: Vec<String>,
}

impl From<DocRecord> for DocHybridRecord {
    fn from(record: DocRecord) -> Self {
        Self {
            summary: record.frontmatter.summary,
            doc_type: Some(record.frontmatter.doc_type.as_str().to_string()),
            tags: record.frontmatter.tags,
        }
    }
}

impl From<IndexedDocFields> for DocHybridRecord {
    fn from(fields: IndexedDocFields) -> Self {
        Self {
            summary: fields.title,
            doc_type: None,
            tags: fields.tags,
        }
    }
}

/// Where one hybrid doc query reads the docs corpus — exactly once either way
/// [DANI-10369].
pub(super) enum DocHybridCorpus<'a> {
    /// The doc chunks in the semantic index: BM25 over `corpus_fts` for the
    /// lexical half, the stored `title`/`tags` for record completion. Nothing
    /// is read from disk.
    Index(&'a VectorStore),
    /// No doc is indexed, so one walk of the docs roots scored the lexical
    /// half and these are the records it saw.
    Walk(BTreeMap<String, DocRecord>),
}

impl DocHybridCorpus<'_> {
    /// The records for `paths`, in whatever order; a path the corpus does not
    /// know is absent.
    pub(super) fn records(
        &self,
        paths: &[&str],
    ) -> Result<BTreeMap<String, DocHybridRecord>, OrbitError> {
        match self {
            Self::Index(store) => Ok(store
                .indexed_doc_fields(paths)?
                .into_iter()
                .map(|(path, fields)| (path, fields.into()))
                .collect()),
            Self::Walk(records) => Ok(paths
                .iter()
                .filter_map(|path| {
                    records
                        .get(*path)
                        .map(|record| ((*path).to_string(), record.clone().into()))
                })
                .collect()),
        }
    }
}

/// An index-served lexical hit in the shape the walk produces, so the rest of
/// the hybrid blend does not care which corpus read answered.
///
/// BM25 exposes rank, not a comparable score, so the score is the hit's
/// position counted from the bottom of `total` hits; the blend min-max
/// normalizes it against the other lexical candidates anyway.
pub(super) fn indexed_doc_result(
    hit: DocLexicalHit,
    record: &DocHybridRecord,
    total: usize,
) -> DocSearchResult {
    DocSearchResult {
        record: DocSearchSource {
            path: hit.source_id,
            doc_type: String::new(),
            summary: record.summary.clone(),
            tags: record.tags.clone(),
            paths: Vec::new(),
            related_features: Vec::new(),
            related_artifacts: Vec::new(),
            body: String::new(),
        },
        score: total.saturating_sub(hit.rank).saturating_add(1),
        matched_by: vec![hit.best_field],
        snippet: Some(hit.snippet),
    }
}

#[derive(Debug, Clone)]
pub(super) struct DocHybridCandidate {
    pub(super) hit: GlobalSearchHit,
    pub(super) lexical_score: Option<f32>,
    pub(super) semantic_score: Option<f32>,
    pub(super) semantic: Option<DocSemanticHit>,
}

pub(super) fn lexical_doc_hits(
    lexical_docs: Vec<orbit_search::DocSearchResult>,
    limit: usize,
) -> Vec<GlobalSearchHit> {
    let mut out = lexical_docs
        .into_iter()
        .map(|result| doc_result_to_global(result.clone(), "lexical", Some(result.score as f32)))
        .collect::<Vec<_>>();
    out.truncate(limit);
    out
}

pub(super) fn blend_doc_hybrid_candidates(
    candidates: Vec<DocHybridCandidate>,
    semantic_weight: f32,
) -> Vec<GlobalSearchHit> {
    let lexical_scores = normalized_doc_scores(candidates.iter().filter_map(|candidate| {
        candidate
            .hit
            .path
            .as_ref()
            .zip(candidate.lexical_score)
            .map(|(path, score)| (path.clone(), score))
    }));
    let semantic_scores = normalized_doc_scores(candidates.iter().filter_map(|candidate| {
        candidate
            .hit
            .path
            .as_ref()
            .zip(candidate.semantic_score)
            .map(|(path, score)| (path.clone(), score))
    }));
    let lexical_weight = 1.0 - semantic_weight;
    let mut out = candidates
        .into_iter()
        .map(|mut candidate| {
            let path = candidate.hit.path.as_deref().unwrap_or_default();
            let lexical = lexical_scores.get(path).copied().unwrap_or(0.0);
            let semantic = semantic_scores.get(path).copied().unwrap_or(0.0);
            let score = semantic_weight.mul_add(semantic, lexical_weight * lexical);
            candidate.hit.score = Some(score);
            if let Some(semantic_hit) = candidate.semantic {
                candidate.hit.best_field = Some(semantic_hit.best_field);
                candidate.hit.snippet = Some(semantic_hit.snippet);
            }
            candidate.hit
        })
        .collect::<Vec<_>>();
    out.sort_by(compare_global_hits_by_score);
    out
}

fn normalized_doc_scores(scores: impl IntoIterator<Item = (String, f32)>) -> BTreeMap<String, f32> {
    let raw = scores.into_iter().collect::<Vec<_>>();
    if raw.len() < 2 {
        return raw.into_iter().collect();
    }
    let min = raw
        .iter()
        .map(|(_, score)| *score)
        .fold(f32::INFINITY, f32::min);
    let max = raw
        .iter()
        .map(|(_, score)| *score)
        .fold(f32::NEG_INFINITY, f32::max);
    if (max - min).abs() <= f32::EPSILON {
        return raw.into_iter().map(|(path, _score)| (path, 1.0)).collect();
    }
    raw.into_iter()
        .map(|(path, score)| (path, (score - min) / (max - min)))
        .collect()
}

pub(super) fn compare_global_hits_by_score(
    left: &GlobalSearchHit,
    right: &GlobalSearchHit,
) -> std::cmp::Ordering {
    right
        .score
        .unwrap_or(0.0)
        .total_cmp(&left.score.unwrap_or(0.0))
        .then_with(|| {
            left.path
                .as_deref()
                .unwrap_or_default()
                .cmp(right.path.as_deref().unwrap_or_default())
        })
        .then_with(|| {
            left.id
                .as_deref()
                .unwrap_or_default()
                .cmp(right.id.as_deref().unwrap_or_default())
        })
}

pub(super) fn warn_doc_hybrid_fallback(notes: &mut Vec<String>, reason: &str) {
    orbit_common::tracing::warn!(
        target: "orbit.search.docs",
        reason,
        "falling back to lexical doc search"
    );
    push_skip_note(
        notes,
        "doc hybrid vector",
        &format!("{DOC_HYBRID_FALLBACK_NOTE}: {reason}"),
    );
}

pub(super) fn warn_task_hybrid_fallback(notes: &mut Vec<String>, reason: &str) {
    orbit_common::tracing::warn!(
        target: "orbit.search.tasks",
        reason,
        "falling back to lexical task search"
    );
    push_skip_note(
        notes,
        "task hybrid vector",
        &format!("{TASK_HYBRID_FALLBACK_NOTE}: {reason}"),
    );
}

/// The companion's install remediation is appropriate for an explicit
/// semantic command, but hybrid search is intentionally best-effort. Keep
/// the fallback note useful without turning an optional dependency into an
/// unattended action item.
pub(super) fn fallback_reason(error: &OrbitError) -> String {
    match error {
        OrbitError::CompanionNotInstalled(_) => {
            "optional inference companion unavailable".to_string()
        }
        OrbitError::Store(message)
            if message
                .contains("semantic index layout is incompatible with this Orbit runtime") =>
        {
            message.clone()
        }
        _ => error.to_string(),
    }
}

pub(super) fn push_skip_note(notes: &mut Vec<String>, branch: &str, reason: &str) {
    notes.push(format!("{branch} branch skipped: {reason}"));
}
