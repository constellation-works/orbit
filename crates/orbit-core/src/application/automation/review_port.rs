//! What delivery evaluation and after-landing review need from review
//! [ORB-17939].
//!
//! Evaluation excludes the landings an accepted before-PR certificate
//! covers, and the `delivery-code-review` consumer draws the crew of the
//! review tasks it mints from `workflow.purpose_crews.review`. `review` owns
//! the coverage rule and the reviewer pools, and it also reads automation
//! state and sources, so a direct call from here would close a cycle. This
//! port is the way in: `review` implements it on
//! [`OrbitRuntime`](crate::OrbitRuntime), so composing the runtime binds it.

use std::collections::BTreeSet;

use orbit_automation::AutomationError;
use orbit_common::OrbitError;
use orbit_types::workflow::ReviewCrewPoolMember;
use orbit_types::workflow::automation::recovery::OsExcludedCrew;
use orbit_types::workflow::automation::{AutomationState, SourcePage};

use super::source::Source;
use crate::application::contracts::{CrewCandidate, CrewPoolDraw};

/// Review coverage and reviewer-pool reads automation code calls,
/// implemented by `review`.
pub(in crate::application) trait AutomationReviewPort {
    /// Add to `page` the exclusions of the landings an accepted before-PR
    /// certificate proves reviewed, or that need no review.
    /// A dry run defers mapped-tag decisions: they can run repository guards
    /// and record task history only during real evaluation.
    fn certified_review_exclusions(
        &self,
        source: &Source<'_>,
        state: &AutomationState,
        page: &mut SourcePage,
        dry_run: bool,
    ) -> Result<(), AutomationError>;

    /// The entries of review pool `key` as canonical, weighed members.
    fn review_pool_members(
        &self,
        entries: &[String],
        key: &str,
    ) -> Result<Vec<ReviewCrewPoolMember>, OrbitError>;

    /// The positive-ticket members of `pool` whose executor can run under
    /// the `os:` tags in `tags`, and the excluded rest.
    fn review_pool_split_by_os(
        &self,
        pool: Vec<ReviewCrewPoolMember>,
        tags: &[String],
    ) -> Result<(Vec<ReviewCrewPoolMember>, Vec<OsExcludedCrew>), OrbitError>;

    /// The members of `pool` this host can run, each with its tickets; with
    /// none left, the refusal of the first member that holds a ticket.
    fn review_pool_candidates(
        &self,
        pool: &[ReviewCrewPoolMember],
    ) -> Result<Vec<CrewCandidate>, OrbitError>;

    /// Draw the crew of an after-landing review task from the members
    /// `eligible` kept, preferring one none of `implementers` names.
    fn draw_after_landing_reviewer(
        &self,
        eligible: &[ReviewCrewPoolMember],
        implementers: &BTreeSet<String>,
        random: &mut impl FnMut() -> Result<u64, OrbitError>,
    ) -> Result<CrewPoolDraw, OrbitError>;
}
