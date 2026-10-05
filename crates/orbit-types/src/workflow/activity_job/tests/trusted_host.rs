use serde_json::json;

use crate::workflow::activity_job::{
    TRUSTED_HOST_ADMISSION_KEY, TrustedHostAdmission, run_input_declares_trusted_host,
};

#[test]
fn a_malformed_admission_still_counts_as_declaring_the_reserved_key() {
    // The submission guard and the engine ask different questions on purpose:
    // a caller who supplies garbage under the reserved key is forging one and
    // must be refused, while the engine must not treat garbage as an admission.
    let input = json!({ TRUSTED_HOST_ADMISSION_KEY: "not-an-admission" });
    assert!(run_input_declares_trusted_host(&input));
    assert!(TrustedHostAdmission::from_run_input(&input).is_none());
}
