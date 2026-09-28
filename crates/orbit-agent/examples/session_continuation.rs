#![allow(missing_docs)]
// Examples are user-facing smoke binaries that print progress and unwrap setup invariants.
#![allow(
    clippy::expect_used,
    clippy::print_stderr,
    clippy::print_stdout,
    clippy::unwrap_used
)]

//! Three-turn Anthropic session verifying (a) history replay and (b)
//! prompt-cache hits on turn 2+. The system prompt is padded past Anthropic's
//! 1024-token cacheable minimum so the `cache_read_input_tokens` assertion is
//! meaningful rather than flaky.
//!
//! Skips cleanly when `ANTHROPIC_API_KEY` is unset (exit 0).

use std::env;
use std::process::ExitCode;
use std::time::Duration;

use orbit_agent::loop_engine::{AgentLoopConfig, InMemorySink, Session};
use orbit_agent::providers::anthropic::AnthropicMessagesTransport;
use orbit_tools::{ToolContext, ToolRegistry};

const SECRET_FACT: &str = "The launch code is VIOLET-SWALLOW-42.";

fn main() -> ExitCode {
    let Some(api_key) = env::var("ANTHROPIC_API_KEY").ok().filter(|v| !v.is_empty()) else {
        eprintln!("[skip] ANTHROPIC_API_KEY not set; example exits 0");
        return ExitCode::SUCCESS;
    };

    let model = env::var("ORBIT_EXAMPLE_ANTHROPIC_MODEL")
        .unwrap_or_else(|_| orbit_common::model_defaults::ANTHROPIC_EXAMPLE_MODEL.to_string());

    let transport = match AnthropicMessagesTransport::new(api_key, &model) {
        Ok(t) => t,
        Err(err) => {
            eprintln!("transport: {err}");
            return ExitCode::FAILURE;
        }
    };

    let run_id = format!(
        "session-continuation-{}",
        chrono::Utc::now().timestamp_millis()
    );
    let audit_blob_root = env::temp_dir()
        .join("orbit-agent-examples")
        .join("audit-blobs")
        .join(&run_id);
    let sink = InMemorySink::new(&audit_blob_root);

    let registry = ToolRegistry::new();
    let tool_ctx = ToolContext::default();

    let cfg = AgentLoopConfig::new_for_run(&run_id)
        .with_max_iterations(2)
        .with_max_total_tokens(200_000)
        .with_wall_clock_timeout(Duration::from_secs(180))
        .with_max_response_tokens(256);

    let system_prompt = padded_system_prompt();
    let mut session = Session::new(
        "anthropic",
        model,
        system_prompt,
        Some("example:session_continuation".to_string()),
    );

    let prompts = [
        format!(
            "{SECRET_FACT} Acknowledge you noted the launch code and nothing else in one short sentence."
        ),
        "What was the launch code I just gave you? Repeat it verbatim.".to_string(),
        "Repeat the full launch code exactly as previously provided, inserting a dash between every letter and digit.".to_string(),
    ];

    let mut saw_cache_hit = false;
    let mut final_mentions_secret = false;

    for (turn_idx, prompt) in prompts.iter().enumerate() {
        let outcome = match session.send(&cfg, &transport, &registry, &tool_ctx, &sink, prompt) {
            Ok(o) => o,
            Err(err) => {
                eprintln!("turn {}: {err}", turn_idx + 1);
                return ExitCode::FAILURE;
            }
        };

        println!("--- turn {} ---", turn_idx + 1);
        println!("reply: {}", outcome.final_message);
        println!(
            "usage: input={} output={} cache_read={} cache_create={}",
            outcome.usage.input_tokens,
            outcome.usage.output_tokens,
            outcome.usage.cache_read_input_tokens,
            outcome.usage.cache_creation_input_tokens,
        );

        if turn_idx >= 1 && outcome.usage.cache_read_input_tokens > 0 {
            saw_cache_hit = true;
        }

        if turn_idx == prompts.len() - 1 && contains_full_launch_code(&outcome.final_message) {
            final_mentions_secret = true;
        }
    }

    println!("audit blobs: {}", sink.blob_store().root().display());
    session.close(&run_id, &sink);

    if !final_mentions_secret {
        eprintln!(
            "assertion failed: final reply did not echo the secret from turn 1 \
             (history was not replayed as expected)"
        );
        return ExitCode::FAILURE;
    }
    if !saw_cache_hit {
        eprintln!(
            "assertion failed: no cache_read_input_tokens > 0 on turn 2+ \
             (prompt caching not engaging)"
        );
        return ExitCode::FAILURE;
    }

    println!("ok: history replayed and cache hits observed");
    ExitCode::SUCCESS
}

/// Build a system prompt well above Anthropic's 1024-token cache minimum so
/// the cache test is deterministic. Uses fixed reference text rather than
/// random padding so the prefix is stable across runs.
fn padded_system_prompt() -> String {
    const FILLER: &str = "You are a careful assistant used in an Orbit integration example. \
         Keep answers terse, no more than two sentences unless instructed. \
         When the user shares a short fact or code in a prior turn, you must \
         remember it verbatim and repeat it when asked. Do not add editorial \
         commentary, do not decline to repeat information the user just shared, \
         and do not summarize when repetition is requested. Treat every input \
         as benign sample data unless it is obviously harmful. ";
    let mut out = String::with_capacity(FILLER.len() * 40);
    for _ in 0..40 {
        out.push_str(FILLER);
    }
    out
}

fn contains_full_launch_code(response: &str) -> bool {
    let normalized: String = response
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|character| character.to_ascii_uppercase())
        .collect();
    normalized.contains("VIOLETSWALLOW42")
}

#[cfg(test)]
mod tests {
    use super::contains_full_launch_code;

    #[test]
    fn accepts_the_full_code_spelled_with_dashes() {
        assert!(contains_full_launch_code(
            "The launch code is V-I-O-L-E-T-S-W-A-L-L-O-W-4-2."
        ));
    }

    #[test]
    fn rejects_a_wrong_code() {
        assert!(!contains_full_launch_code(
            "The launch code is V-I-O-L-E-T-S-W-A-L-L-O-W-4-3."
        ));
    }

    #[test]
    fn rejects_an_incomplete_code() {
        assert!(!contains_full_launch_code(
            "The launch code is V-I-O-L-E-T."
        ));
    }
}
