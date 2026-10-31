use orbit_types::assistance::AssistanceReason;

#[derive(Clone, Copy)]
pub(super) enum DecisionsSendFailureClass {
    Timeout,
    Connect,
    Builder,
    Other,
}

pub(super) fn decisions_send_failure_reason(
    failure: DecisionsSendFailureClass,
) -> AssistanceReason {
    match failure {
        DecisionsSendFailureClass::Connect | DecisionsSendFailureClass::Builder => {
            AssistanceReason::Unavailable
        }
        DecisionsSendFailureClass::Timeout | DecisionsSendFailureClass::Other => {
            AssistanceReason::TimeoutUnknown
        }
    }
}
