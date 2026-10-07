use std::path::Path;

use super::super::baseline::failure_identities;

// The output grammars of each runner meet the combinatorial pure-logic
// unit-test criterion: the boundary test drives one of them.
#[test]
fn failure_identities_name_each_failed_test_and_located_error_across_runners() {
    let root = Path::new("/work/run-a");
    let output = "\
running 3 tests
test suite::ok ... ok
test suite::broken ... FAILED
\u{1b}[31m        FAIL\u{1b}[0m [   0.004s] (2/9) orbit-core runtime::red
     TIMEOUT [  60.001s] orbit-core runtime::slow
       PASS [   0.002s] orbit-core runtime::fine
error[E0308]: mismatched types
  --> /work/run-a/src/lib.rs:4:9
warning: unused variable
  --> src/lib.rs:7:1
error: an error with no location
error: this `if` has identical blocks
   --> src/main.rs:10:5
";
    let found = failure_identities(output, root);
    assert_eq!(
        found.into_iter().collect::<Vec<_>>(),
        vec![
            "src/lib.rs:4:9: error[E0308]: mismatched types",
            "src/main.rs:10:5: error: this `if` has identical blocks",
            "test orbit-core runtime::red",
            "test orbit-core runtime::slow",
            "test suite::broken",
        ],
        "an error is located under its own root; warnings and passes are not failures"
    );
    // The same failures from another checkout are the same identities.
    assert_eq!(
        failure_identities(
            &output.replace("/work/run-a", "/base/tree"),
            Path::new("/base/tree")
        ),
        failure_identities(output, root)
    );
}
