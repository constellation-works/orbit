//! The provenance tags system filings carry. The sweeps and review
//! reconciliation stamp them on the tasks they file, and job's crew pools
//! route those tasks to the sweep purpose by them [ORB-17938]. Task records
//! persist these strings, so their values never change.

/// Every task the CI-failure sweep files carries it.
pub(crate) const CI_FAILURE_TAG: &str = "ci-failure-sweep";

/// Every task the Dependabot sweep files carries it.
pub(crate) const DEPENDABOT_TAG: &str = "dependabot-sweep";

/// Every task the code-scanning sweep files carries it.
pub(crate) const CODE_SCANNING_TAG: &str = "code-scanning-sweep";

/// Prefix of the tag carried by the follow-up task a review reconciliation
/// files, completed by the reconciliation id.
pub(crate) const RECONCILIATION_FOLLOW_UP_TAG_PREFIX: &str = "review-reconciliation:";
