//! Shared argument parsers.

use crate::parse::positive_limit;

#[test]
fn positive_limit_accepts_counts_and_refuses_zero_and_non_numbers() {
    assert_eq!(positive_limit("1"), Ok(1));
    assert_eq!(positive_limit("250"), Ok(250));
    assert!(positive_limit("0").is_err(), "zero must be refused");
    assert!(positive_limit("-3").is_err(), "negative must be refused");
    assert!(positive_limit("many").is_err(), "non-numbers must be refused");
}
