/// Normalize the review spellings shared by task-update admission and the merge gate.
/// Unknown values are returned trimmed and uppercased for the caller to reject.
pub fn normalize_review_decision(value: &str) -> String {
    match value.trim().to_ascii_uppercase().as_str() {
        "APPROVED" | "APPROVE" => "APPROVED".to_string(),
        "REQUEST-CHANGES" | "REQUEST_CHANGES" | "CHANGES-REQUESTED" | "CHANGES_REQUESTED" => {
            "CHANGES_REQUESTED".to_string()
        }
        other => other.to_string(),
    }
}
