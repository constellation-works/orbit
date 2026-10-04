use orbit_common::OrbitError;
use orbit_store::friction_store::FrictionListFilter;

use crate::OrbitRuntime;

use super::filters::SearchStatusFilters;
use super::{GlobalSearchHit, GlobalSearchParams};

impl OrbitRuntime {
    /// Friction hits the filters admit, and whether more matched than
    /// `limit`. A listing spans every status unless a `friction:` status
    /// narrows it and carries each full record; a query defaults to open
    /// records unless `all` widens it.
    pub(super) fn friction_branch(
        &self,
        params: &GlobalSearchParams,
        status_filters: &SearchStatusFilters,
        query: Option<&str>,
        tag_filter: &[String],
        limit: usize,
    ) -> Result<(Vec<GlobalSearchHit>, bool), OrbitError> {
        let listing = params.is_friction_listing();
        let status = status_filters
            .friction
            .or((!params.all && !listing).then_some(orbit_types::record::FrictionStatus::Open));
        let records = crate::runtime::friction::store_for(self)?.list(&FrictionListFilter {
            status,
            q: query.map(str::to_string),
            limit: None,
            ..FrictionListFilter::default()
        })?;

        let mut admitted = records.into_iter().filter(|stored| {
            tag_filter.iter().all(|needle| {
                stored
                    .record
                    .tags
                    .iter()
                    .any(|candidate| candidate.eq_ignore_ascii_case(needle))
            })
        });
        let mut hits = Vec::new();
        for stored in admitted.by_ref().take(limit) {
            let record = stored.record;
            let title = orbit_common::governance::friction::effective_title(
                record.title.as_deref(),
                &record.body,
                &record.id,
            );
            let full = if listing {
                let mut value = serde_json::to_value(&record).map_err(|error| {
                    OrbitError::Store(format!("serialize friction record: {error}"))
                })?;
                value["title"] = serde_json::json!(title);
                Some(value)
            } else {
                None
            };
            hits.push(GlobalSearchHit {
                kind: "friction".to_string(),
                source: "lexical".to_string(),
                id: Some(record.id.clone()),
                path: None,
                title: Some(title),
                summary: None,
                status: Some(record.status.as_str().to_string()),
                best_field: None,
                snippet: Some(record.body),
                score: None,
                score_breakdown: None,
                matched_by: None,
                workspace: None,
                record: full,
            });
        }
        let truncated = admitted.next().is_some();
        Ok((hits, truncated))
    }
}
