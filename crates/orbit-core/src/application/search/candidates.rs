use std::cell::RefCell;

use orbit_common::OrbitError;
use orbit_search::{SOURCE_KIND_TASK, bm25_or_page, bm25_page};

use crate::OrbitRuntime;
use crate::application::task::TaskListFilter;

use super::convert::{fill_task_record_fields, lexical_task_hit};
use super::filters::{SearchStatusFilters, resolve_task_statuses, task_has_all_tags};
use super::{GlobalSearchHit, GlobalSearchParams, task_selectors_contain_path};

/// The read-only inputs shared by each search-kind branch.
#[derive(Clone, Copy)]
pub(super) struct BranchContext<'a> {
    pub(super) params: &'a GlobalSearchParams,
    pub(super) status_filters: &'a SearchStatusFilters,
    pub(super) query: Option<&'a str>,
    pub(super) tag_filter: &'a [String],
    pub(super) limit: usize,
}

impl OrbitRuntime {
    pub(super) fn task_branch(
        &self,
        ctx: BranchContext<'_>,
    ) -> Result<Vec<GlobalSearchHit>, OrbitError> {
        let BranchContext {
            params,
            status_filters,
            query,
            tag_filter,
            limit,
        } = ctx;
        let statuses = resolve_task_statuses(params, status_filters);
        let path = params.path.as_deref();
        let accepts = |task: &orbit_types::task::Task| {
            statuses.contains(&task.status)
                && (tag_filter.is_empty() || task_has_all_tags(task, tag_filter))
                && path.is_none_or(|path| task_selectors_contain_path(&task.context_files, path))
        };

        let candidates = if let Some(query) = query {
            self.lexical_task_candidates(query, limit, accepts)?
        } else {
            // No query → enumerate tasks (used by `--path` and `--tag`).
            self.accepted_task_page(tag_filter, limit, &accepts)?
                .into_iter()
                .map(|task| (lexical_task_hit(&task), task))
                .collect()
        };

        let mut out = Vec::new();
        for (mut hit, task) in candidates {
            fill_task_record_fields(&mut hit, &task);
            out.push(hit);
        }
        out.truncate(limit);
        Ok(out)
    }

    /// The first `limit` tasks (newest first) carrying every tag in `tags`
    /// that `accepts` admits. `accepts` reads envelope fields only, so the
    /// store judges every candidate from its envelope and hydrates just the
    /// page, instead of reading every task's bundle to discard most of them.
    fn accepted_task_page(
        &self,
        tags: &[String],
        limit: usize,
        accepts: &dyn Fn(&orbit_types::task::Task) -> bool,
    ) -> Result<Vec<orbit_types::task::Task>, OrbitError> {
        if self.worker_invocation().is_some() {
            // The owner answers whole listings only.
            let tasks = if tags.is_empty() {
                self.list_tasks()?
            } else {
                self.list_tasks_by_tags(tags)?
            };
            return Ok(tasks
                .into_iter()
                .filter(|task| accepts(task))
                .take(limit)
                .collect());
        }
        if !self.coordination_task_reads_visible() {
            return Ok(Vec::new());
        }
        let page = self.stores().tasks().query_task_rows(
            &TaskListFilter {
                tags: tags.to_vec(),
                ..TaskListFilter::default()
            },
            limit,
            Some(&|task, _| accepts(task)),
        )?;
        Ok(page.items.into_iter().map(|row| row.task).collect())
    }

    /// Lexical task candidates, at most `2 × limit`, from two sources in order.
    ///
    /// When the lexical index holds task chunks, FTS5 BM25 over
    /// `corpus_fts` ranks first: it matches non-adjacent terms and orders by
    /// relevance. The index carries only title, description, plan,
    /// execution summary, and acceptance criteria, so the bundle matcher then
    /// supplements what FTS cannot see — comments, `external_refs`, artifact
    /// manifest paths, and tasks not yet indexed — appended in index order
    /// behind the BM25 hits [DANI-10445]. Without task chunks the bundle
    /// matcher is the only source. Neither source opens artifact payloads.
    ///
    /// When fewer than `limit` full matches survive, append any-term indexed
    /// hits ordered by the best chunk's matched-term count, then BM25. Partial
    /// labels report that chunk's count; bundle-only fields stay substring-based.
    ///
    /// Only tasks `accepts` admits count toward the candidate budget, so a
    /// status, tag or path filter cannot starve the page when the best
    /// lexical matches are all filtered out: BM25 is read in bounded pages,
    /// continuing past rejected chunks until the budget fills or the ranking
    /// runs out.
    fn lexical_task_candidates(
        &self,
        query: &str,
        limit: usize,
        accepts: impl Fn(&orbit_types::task::Task) -> bool,
    ) -> Result<Vec<(GlobalSearchHit, orbit_types::task::Task)>, OrbitError> {
        let candidate_limit = limit.saturating_mul(2).max(limit);
        let mut seen = std::collections::BTreeSet::new();
        let mut candidates = Vec::with_capacity(candidate_limit);
        if let Ok(index) = self.stores().lexical_index().store()
            && index.has_source_kind(SOURCE_KIND_TASK)?
        {
            // BM25 ranks chunks, while this branch returns tasks. Size each
            // page by the number of task fields normally indexed, then keep
            // the first occurrence of each task in BM25 order.
            let page_size = candidate_limit.saturating_mul(5).max(candidate_limit);
            let mut offset = 0;
            loop {
                let page = bm25_page(
                    index,
                    query,
                    Some(SOURCE_KIND_TASK),
                    None,
                    offset,
                    page_size,
                )?;
                let exhausted = page.len() < page_size;
                for hit in page {
                    if !seen.insert(hit.source_id.clone()) {
                        continue;
                    }
                    if let Some(task) = self.lexical_hit_task(&hit.source_id)
                        && accepts(&task)
                    {
                        candidates.push((lexical_task_hit(&task), task));
                    }
                    if candidates.len() == candidate_limit {
                        return Ok(candidates);
                    }
                }
                if exhausted {
                    break;
                }
                offset += page_size;
            }
        }

        // The bundle matcher supplements the BM25 hits, in index order, until
        // the budget fills. `accepts` and the tasks BM25 already returned are
        // judged from envelopes, so neither costs a bundle read, and the scan
        // stops as soon as the budget is full.
        if candidates.len() < candidate_limit {
            let seen = RefCell::new(&mut seen);
            self.search_tasks_visit(
                query,
                &[],
                &|task| accepts(task) && !seen.borrow().contains(&task.id),
                &mut |task| {
                    if seen.borrow_mut().insert(task.id.clone()) {
                        candidates.push((lexical_task_hit(&task), task));
                    }
                    candidates.len() < candidate_limit
                },
            )?;
        }
        // Full FTS and bundle matches retain priority. Only a short page
        // needs the broader pass, and single-term queries cannot be partial.
        let term_count = query.split_whitespace().count();
        if candidates.len() < limit
            && term_count > 1
            && let Ok(index) = self.stores().lexical_index().store()
            && index.has_source_kind(SOURCE_KIND_TASK)?
        {
            let page_size = limit.saturating_mul(5);
            let mut offset = 0;
            loop {
                let page = bm25_or_page(
                    index,
                    query,
                    Some(SOURCE_KIND_TASK),
                    None,
                    offset,
                    page_size,
                )?;
                let exhausted = page.len() < page_size;
                for hit in page {
                    if !seen.insert(hit.source_id.clone()) {
                        continue;
                    }
                    if let Some(task) = self.lexical_hit_task(&hit.source_id)
                        && accepts(&task)
                    {
                        let mut result = lexical_task_hit(&task);
                        result.matched_by = Some(vec![
                            "partial".into(),
                            format!("terms:{}/{term_count}", hit.matched_terms),
                        ]);
                        candidates.push((result, task));
                        if candidates.len() == limit {
                            return Ok(candidates);
                        }
                    }
                }
                if exhausted {
                    break;
                }
                offset += page_size;
            }
        }
        Ok(candidates)
    }

    /// Hydrate one BM25 hit for its summary and the filters. The listing read
    /// parses the task documents without opening artifact payloads, which the
    /// canonical read would hash for every hit [ORB-14595]; a bundle a
    /// concurrent writer holds, or one that cannot be read, drops the hit as
    /// an unreadable task always has. A worker reads through its owner.
    fn lexical_hit_task(&self, id: &str) -> Option<orbit_types::task::Task> {
        if self.worker_invocation().is_some() {
            return self.get_task(id).ok();
        }
        self.get_listed_task_row(id)
            .ok()
            .flatten()
            .map(|row| row.task)
    }
}
