//! Behavior tests for dashboard task-page query parsing and cursors.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::super::HISTORY_MAX_LIMIT;
use super::super::pagination::TaskPageQuery;

#[test]
fn oversized_limit_is_clamped_to_the_history_maximum() {
    let query = TaskPageQuery::parse(Some("limit=999999999")).expect("parse oversized limit");
    assert_eq!(query.limit(), HISTORY_MAX_LIMIT);
    let query = TaskPageQuery::parse(Some("limit=25")).expect("parse small limit");
    assert_eq!(query.limit(), 25);
}

#[test]
fn zero_and_non_numeric_limits_are_rejected() {
    assert!(TaskPageQuery::parse(Some("limit=0")).is_err());
    assert!(TaskPageQuery::parse(Some("limit=many")).is_err());
}

/// The cursor embeds the limit, so a cursor minted for an oversized request
/// must be accepted by a follow-up carrying the clamped limit and vice versa.
#[test]
fn cursor_minted_for_oversized_limit_matches_the_clamped_limit() {
    let oversized = TaskPageQuery::parse(Some("limit=999999999")).expect("parse");
    let cursor = oversized
        .next_cursor("scope", chrono::Utc::now(), "ORB-1".to_string(), 1)
        .expect("encode cursor");
    let mut follow_up =
        TaskPageQuery::parse(Some(&format!("limit={HISTORY_MAX_LIMIT}&cursor={cursor}")))
            .expect("parse follow-up");
    follow_up.bind_cursor("scope").expect("cursor stays valid");
    assert_eq!(follow_up.offset(), 1);
}
