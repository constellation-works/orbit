//! Review's side of the automation-owned port [ORB-17939].

use std::collections::BTreeSet;

use orbit_automation::AutomationError;
use orbit_common::OrbitError;
use orbit_types::workflow::ReviewCrewPoolMember;
use orbit_types::workflow::automation::recovery::OsExcludedCrew;
use orbit_types::workflow::automation::{AutomationState, SourcePage};

use super::coverage::exclusions;
use super::crew::configured_pool_members;
use crate::OrbitRuntime;
use crate::application::automation::AutomationReviewPort;
use crate::application::automation::source::Source;
use crate::application::contracts::CrewCandidate;
use crate::application::job::crew_pools::CrewPoolDraw;

impl AutomationReviewPort for OrbitRuntime {
    fn certified_review_exclusions(
        &self,
        source: &Source<'_>,
        state: &AutomationState,
        page: &mut SourcePage,
    ) -> Result<(), AutomationError> {
        exclusions(self, source, state, page)
    }

    fn review_pool_members(
        &self,
        entries: &[String],
        key: &str,
    ) -> Result<Vec<ReviewCrewPoolMember>, OrbitError> {
        configured_pool_members(self, entries, key)
    }

    fn review_pool_split_by_os(
        &self,
        pool: Vec<ReviewCrewPoolMember>,
        tags: &[String],
    ) -> Result<(Vec<ReviewCrewPoolMember>, Vec<OsExcludedCrew>), OrbitError> {
        self.review_pool_by_os(pool, tags)
    }

    fn review_pool_candidates(
        &self,
        pool: &[ReviewCrewPoolMember],
    ) -> Result<Vec<CrewCandidate>, OrbitError> {
        self.review_crew_candidates(pool)
    }

    fn draw_after_landing_reviewer(
        &self,
        eligible: &[ReviewCrewPoolMember],
        implementers: &BTreeSet<String>,
        random: &mut impl FnMut() -> Result<u64, OrbitError>,
    ) -> Result<CrewPoolDraw, OrbitError> {
        self.select_review_crew(eligible, implementers, random)
    }
}
