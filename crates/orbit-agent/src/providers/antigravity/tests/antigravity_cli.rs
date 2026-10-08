#![allow(missing_docs)]

use std::time::Duration;

use super::super::antigravity_cli::apply_antigravity_print_timeout;

fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| value.to_string()).collect()
}

#[test]
fn antigravity_reports_the_print_timeout_it_put_in_argv() {
    for provider in ["antigravity", "agy"] {
        let mut derived = args(&["--model", "m"]);
        let chosen =
            apply_antigravity_print_timeout(provider, &mut derived, Duration::from_secs(60));
        assert_eq!(chosen, Some(Duration::from_secs(30)));
        assert!(
            derived
                .windows(2)
                .any(|pair| pair == ["--print-timeout", "30s"])
        );

        let mut shorter = args(&["--print-timeout=10s"]);
        let chosen =
            apply_antigravity_print_timeout(provider, &mut shorter, Duration::from_secs(600));
        assert_eq!(
            chosen,
            Some(Duration::from_secs(10)),
            "a shorter custom value wins"
        );

        let mut longer = args(&["--print-timeout", "2h"]);
        let chosen =
            apply_antigravity_print_timeout(provider, &mut longer, Duration::from_secs(600));
        assert_eq!(
            chosen,
            Some(Duration::from_secs(570)),
            "a longer custom value is capped"
        );
        assert_eq!(longer, args(&["--print-timeout", "9m30s"]));
    }
}

#[test]
fn other_providers_get_no_print_timeout() {
    for provider in ["claude", "codex", "gemini", "grok"] {
        let mut argv = args(&["--model", "m"]);
        let chosen = apply_antigravity_print_timeout(provider, &mut argv, Duration::from_secs(600));
        assert_eq!(chosen, None, "{provider}");
        assert_eq!(
            argv,
            args(&["--model", "m"]),
            "{provider} argv must be untouched"
        );
    }
}
