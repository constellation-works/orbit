use super::super::tee::RedactingEcho;

fn echo(chunks: &[&[u8]]) -> String {
    let mut echo = RedactingEcho::new(Vec::new());
    for chunk in chunks {
        echo.push(chunk);
    }
    String::from_utf8(echo.finish()).expect("utf8 echo")
}

#[test]
fn a_secret_split_across_reads_is_still_redacted() {
    let secret = "orbit-tee-test-secret-value-8c1f";
    let _env = orbit_common::test_env::scoped([("ORBIT_TEE_TEST_TOKEN", Some(secret))]);

    let output = echo(&[b"token=orbit-tee-test-", b"secret-value-8c1f\n"]);

    assert!(!output.contains(secret), "{output}");
    assert!(output.starts_with("token="), "{output}");
}

#[test]
fn a_character_split_across_reads_is_echoed_intact() {
    let output = echo(&[b"caf\xc3", b"\xa9\n", b"tail without newline"]);

    assert_eq!(output, "café\ntail without newline");
}
