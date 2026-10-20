/// Normalize PR review aliases for both merge decisions and task-write admission.
/// Unknown values are uppercased and preserved so callers can refuse them.
pub fn normalize_review_decision(value: &str) -> String {
    match value.trim().to_ascii_uppercase().as_str() {
        "APPROVED" | "APPROVE" => "APPROVED".to_string(),
        "REQUEST-CHANGES" | "REQUEST_CHANGES" | "CHANGES-REQUESTED" | "CHANGES_REQUESTED" => {
            "CHANGES_REQUESTED".to_string()
        }
        other => other.to_string(),
    }
}
