use crate::task::{UNSET_BUCKET, complexity_bucket, complexity_bucket_ord, labeled_or_unset};

#[test]
fn empty_and_missing_labels_become_unset() {
    assert_eq!(labeled_or_unset(None), UNSET_BUCKET);
    assert_eq!(labeled_or_unset(Some("")), UNSET_BUCKET);
    assert_eq!(labeled_or_unset(Some("   ")), UNSET_BUCKET);
    assert_eq!(labeled_or_unset(Some("hard")), "hard");
}

#[test]
fn unassessed_and_missing_complexity_share_one_bucket() {
    assert_eq!(complexity_bucket(None), UNSET_BUCKET);
    assert_eq!(complexity_bucket(Some("")), UNSET_BUCKET);
    assert_eq!(complexity_bucket(Some("unassessed")), UNSET_BUCKET);
    assert_eq!(complexity_bucket(Some("hard")), "hard");
}

#[test]
fn unexpected_labels_are_not_folded_into_a_known_band() {
    assert_eq!(labeled_or_unset(Some("extreme")), "extreme");
    assert_eq!(complexity_bucket(Some("extreme")), "extreme");
    assert!(complexity_bucket_ord("unset") < complexity_bucket_ord("low"));
    assert!(complexity_bucket_ord("hard") < complexity_bucket_ord("xhard"));
    assert!(complexity_bucket_ord("xhard") < complexity_bucket_ord("extreme"));
}
