use std::ffi::OsStr;

#[test]
fn inherited_strict_policy_applies_to_child_worker_launches() {
    let strict = super::super::start::effective_strict_containment;
    assert!(!strict(false, false, None));
    assert!(strict(true, false, None));
    assert!(strict(false, true, None));
    assert!(strict(false, false, Some(OsStr::new("1"))));
    assert!(!strict(false, false, Some(OsStr::new("0"))));
}
