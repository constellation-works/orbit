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

        // [ORB-15206] The budget is the deadline Orbit enforces minus the 30 s
        // margin, not a multiple of the runtime.
        let mut hour = args(&[]);
        let chosen =
            apply_antigravity_print_timeout(provider, &mut hour, Duration::from_secs(3600));
        assert_eq!(chosen, Some(Duration::from_secs(3570)));
        assert_eq!(hour, args(&["--print-timeout", "59m30s"]));

        for (deadline_secs, expected) in [(0, "1s"), (30, "30s"), (31, "1s"), (4200, "1h9m30s")] {
            let mut argv = Vec::new();
            apply_antigravity_print_timeout(
                provider,
                &mut argv,
                Duration::from_secs(deadline_secs),
            );
            assert_eq!(argv, args(&["--print-timeout", expected]));
        }
        let mut fractional = args(&["--print-timeout=1.5s"]);
        assert_eq!(
            apply_antigravity_print_timeout(provider, &mut fractional, Duration::from_secs(60)),
            Some(Duration::from_millis(1500)),
        );
        assert_eq!(fractional, args(&["--print-timeout=1.5s"]));

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
