use orbit_common::OrbitError;
use orbit_types::{
    desktop::{DesktopReviewDecision, DesktopReviewVerdict},
    task::{GITHUB_PR_EXTERNAL_REF_SYSTEM, Task, TaskStatus},
};

use crate::OrbitRuntime;

use super::validation::{bounded_text, criteria, invalid};

impl OrbitRuntime {
    pub(super) fn desktop_validate_verdict(
        &self,
        task: &Task,
        verdict: &DesktopReviewVerdict,
    ) -> Result<(), OrbitError> {
        if task.status != TaskStatus::Review {
            return Err(invalid("review decisions require review state"));
        }
        bounded_text(&verdict.rationale, true)?;
        if task.job_run_id != verdict.expected_run_id {
            return Err(invalid("reviewed run binding changed"));
        }
        criteria(&task.acceptance_criteria)?;
        if verdict.criteria.len() != task.acceptance_criteria.len()
            || verdict.evidence.is_empty()
            || verdict.evidence.len() > 100
        {
            return Err(invalid(
                "review verdict must cover every criterion and cite current evidence",
            ));
        }
        let manifest = self.get_task_artifact_manifest(&task.id)?;
        let known = |e: &str| {
            e == "execution_summary" && !task.execution_summary.trim().is_empty()
                || manifest.iter().any(|a| a.path == e)
                || task
                    .external_refs
                    .iter()
                    .any(|r| r.url.as_deref() == Some(e))
                || task.job_run_id.as_deref() == Some(e)
        };
        for e in &verdict.evidence {
            if !known(e) {
                return Err(invalid(
                    "review evidence must reference current summary, run, artifact or external reference",
                ));
            }
        }
        for (expected, actual) in task.acceptance_criteria.iter().zip(&verdict.criteria) {
            if &actual.criterion != expected
                || actual.evidence.is_empty()
                || actual.evidence.len() > 100
                || actual.evidence.iter().any(|e| !known(e))
                || (verdict.decision == DesktopReviewDecision::Accept && !actual.met)
            {
                return Err(invalid(
                    "criterion outcome is missing, unmet, or references unavailable evidence",
                ));
            }
        }
        Ok(())
    }
    pub(super) fn desktop_observe_pr_head(
        &self,
        task: &Task,
        verdict: &DesktopReviewVerdict,
    ) -> Result<Option<String>, OrbitError> {
        let head = self.desktop_current_pr_head(task)?;
        if verdict.expected_head != head {
            return Err(invalid("linked PR head changed since review"));
        }
        Ok(head)
    }
    pub(super) fn desktop_current_pr_head(
        &self,
        task: &Task,
    ) -> Result<Option<String>, OrbitError> {
        let refs: Vec<_> = task
            .external_refs
            .iter()
            .filter(|r| r.system == GITHUB_PR_EXTERNAL_REF_SYSTEM)
            .collect();
        if refs.is_empty() {
            return Ok(None);
        }
        if refs.len() != 1 {
            return Err(invalid(
                "desktop completion requires one unambiguous PR reference",
            ));
        }
        let url = refs[0]
            .url
            .as_deref()
            .ok_or_else(|| invalid("linked PR has no verifiable URL"))?;
        let value = self.run_tool(
            "github.pr.list",
            serde_json::json!({"state":"all","limit":100}),
        )?;
        let head = value["pull_requests"]
            .as_array()
            .and_then(|rows| rows.iter().find(|r| r["url"].as_str() == Some(url)))
            .and_then(|r| r["reported_head_sha"].as_str())
            .filter(|h| !h.is_empty())
            .ok_or_else(|| {
                invalid("current linked PR head unavailable; refresh repository evidence")
            })?;
        Ok(Some(head.into()))
    }
}
