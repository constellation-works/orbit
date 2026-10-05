use crate::task::{TaskError, validate_task_dependencies_with};

fn chain_lookup(
    len: usize,
    closes_cycle: bool,
) -> impl FnMut(&str) -> Result<Option<Vec<String>>, TaskError> {
    move |id| {
        let number: usize = id
            .strip_prefix("ORB-")
            .and_then(|number| number.parse().ok())
            .expect("chain ids are numeric");
        Ok(Some(if number < len {
            vec![format!("ORB-{}", number + 1)]
        } else if closes_cycle {
            vec!["ORB-0".to_string()]
        } else {
            Vec::new()
        }))
    }
}

const LONG_CHAIN: usize = 200_000;

#[test]
fn cycle_at_end_of_long_chain_reports_full_witness() {
    let error = validate_task_dependencies_with(
        Some("ORB-0"),
        &["ORB-1".to_string()],
        chain_lookup(LONG_CHAIN, true),
    )
    .expect_err("last task closes the cycle");
    let message = error.to_string();
    assert!(
        message.contains("task dependency cycle detected: ORB-0 -> ORB-1 -> ORB-2 -> "),
        "{}",
        &message[..message.len().min(200)]
    );
    assert!(
        message.ends_with(&format!(
            "ORB-{} -> ORB-{LONG_CHAIN} -> ORB-0",
            LONG_CHAIN - 1
        )),
        "{}",
        &message[message.len().saturating_sub(200)..]
    );
}
