#![allow(missing_docs)]

mod args {
    #![allow(missing_docs)]

    use super::super::super::grok_cli::*;
    use orbit_types::identity::ReasoningEffort;

    #[test]
    fn grok_args_pass_model_with_long_flag() {
        let transport = GrokCliTransport::new(Some("grok-4.6".to_string()), None);

        assert_eq!(transport.args(), vec!["--model", "grok-4.6"]);
    }

    #[test]
    fn grok_args_pass_supported_effort_with_reasoning_effort_flag() {
        let transport =
            GrokCliTransport::new(Some("grok-4.6".to_string()), Some(ReasoningEffort::Xhigh));

        assert_eq!(
            transport.args(),
            vec!["--model", "grok-4.6", "--reasoning-effort", "xhigh"]
        );
    }
}
