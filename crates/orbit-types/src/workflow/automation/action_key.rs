/// The persisted action key of one automation attempt, `automation:<id>:<attempt>`.
///
/// Writers, store transition validators and the task projection all derive the key
/// here. The key is persisted and indexed as the task action key, so its bytes must
/// not change.
pub fn attempt_action_key(id: &str, attempt: u32) -> String {
    format!("automation:{id}:{attempt}")
}
