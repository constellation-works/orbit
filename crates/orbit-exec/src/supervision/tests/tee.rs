use super::super::tee::{MAX_PENDING_ECHO_BYTES, RedactingEcho};

fn echo(chunks: &[&[u8]]) -> String {
    let mut echo = RedactingEcho::new(Vec::new());
    for chunk in chunks {
        echo.push(chunk);
        assert!(
            echo.buffered_len() <= MAX_PENDING_ECHO_BYTES,
            "pending {} after a push",
            echo.buffered_len()
        );
    }
    String::from_utf8(echo.finish()).expect("utf8 echo")
}

fn echo_chunked(bytes: &[u8], chunk_size: usize) -> String {
    assert!(chunk_size > 0, "chunk size");
    let mut echo = RedactingEcho::new(Vec::new());
    let mut pushed = 0usize;
    let mut flushed_past_bound = false;
    for chunk in bytes.chunks(chunk_size) {
        echo.push(chunk);
        pushed += chunk.len();
        let buffered = echo.buffered_len();
        assert!(
            buffered <= MAX_PENDING_ECHO_BYTES,
            "chunk {chunk_size}: pending {buffered} after {pushed} bytes"
        );
        if pushed > MAX_PENDING_ECHO_BYTES && buffered < pushed {
            flushed_past_bound = true;
        }
    }
    if bytes.len() > MAX_PENDING_ECHO_BYTES {
        assert!(
            flushed_past_bound,
            "chunk {chunk_size}: input larger than the flush bound was held in full"
        );
    }
    String::from_utf8(echo.finish()).expect("utf8 echo")
}

fn boundary_payload(secret: &str) -> Vec<u8> {
    let secret = secret.as_bytes();
    assert!(secret.len() >= 2, "secret needs two halves");
    assert!(
        secret.len() <= MAX_PENDING_ECHO_BYTES,
        "this fixture stays within the flush bound"
    );
    assert!(!secret.contains(&b'\n'), "newline-free flush fixture");
    let split_at = secret.len() / 2;
    let secret_at = MAX_PENDING_ECHO_BYTES - split_at;
    let head = b"[[head]]";
    let tail = b"[[tail]]";
    assert!(head.len() < secret_at, "head fits before the secret");
    let mut payload = Vec::with_capacity(secret_at + secret.len() + tail.len());
    payload.extend_from_slice(head);
    payload.resize(secret_at, b'x');
    payload.extend_from_slice(secret);
    payload.extend_from_slice(tail);
    assert_eq!(&payload[secret_at..secret_at + secret.len()], secret);
    assert!(secret_at < MAX_PENDING_ECHO_BYTES);
    assert!(secret_at + secret.len() > MAX_PENDING_ECHO_BYTES);
    payload
}

fn assert_secret_replaced(secret: &str, payload: &[u8], output: &str, chunk: usize) {
    assert!(
        !output.contains(secret),
        "chunk {chunk}: echoed the complete secret"
    );
    let split_at = secret.len() / 2;
    let secret_at = MAX_PENDING_ECHO_BYTES - split_at;
    let mut expected = payload.to_vec();
    expected.splice(
        secret_at..secret_at + secret.len(),
        b"[REDACTED_ENV]".iter().copied(),
    );
    if output.as_bytes() != expected.as_slice() {
        let mismatch = output
            .bytes()
            .zip(expected.iter().copied())
            .position(|(got, want)| got != want);
        panic!(
            "chunk {chunk}: output len {} expected len {} first mismatch {mismatch:?}",
            output.len(),
            expected.len()
        );
    }
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

#[test]
fn a_secret_split_across_the_forced_flush_is_redacted_for_many_chunkings() {
    let secrets = ["orbit-tee-flush-boundary-secret-8c1f", "abcabcabcabc"];
    let chunkings = [
        1usize,
        2,
        3,
        7,
        64,
        4095,
        4096,
        8191,
        8192,
        MAX_PENDING_ECHO_BYTES - 1,
        MAX_PENDING_ECHO_BYTES,
        MAX_PENDING_ECHO_BYTES + 1,
    ];
    for secret in secrets {
        let _env = orbit_common::test_env::scoped([("ORBIT_TEE_FLUSH_TOKEN", Some(secret))]);
        let payload = boundary_payload(secret);
        for chunk in chunkings {
            let output = echo_chunked(&payload, chunk);
            assert_secret_replaced(secret, &payload, &output, chunk);
        }
        let output = echo_chunked(&payload, payload.len());
        assert_secret_replaced(secret, &payload, &output, payload.len());
    }
}

#[test]
fn a_multiline_secret_split_on_its_newline_is_redacted() {
    let secret = "orbit-tee-multi-head-8c1f\norbit-tee-multi-tail-8c1f";
    let _env = orbit_common::test_env::scoped([("ORBIT_TEE_MULTI_TOKEN", Some(secret))]);
    let (first, second) = secret.split_once('\n').expect("multiline fixture");
    let mut payload = Vec::new();
    payload.extend_from_slice(b"BEFORE-");
    payload.extend_from_slice(secret.as_bytes());
    payload.extend_from_slice(b"-AFTER\n");
    let expected = "BEFORE-[REDACTED_ENV]-AFTER\n";

    let chunkings = [1usize, 4, first.len(), first.len() + 1, payload.len()];
    for chunk in chunkings {
        let output = echo_chunked(&payload, chunk);
        assert!(
            !output.contains(secret),
            "chunk {chunk}: echoed the complete secret"
        );
        assert!(
            !output.contains(first),
            "chunk {chunk}: echoed the secret head"
        );
        assert!(
            !output.contains(second),
            "chunk {chunk}: echoed the secret tail"
        );
        assert_eq!(output, expected, "chunk {chunk}");
    }

    let output = echo(&[
        b"BEFORE-",
        first.as_bytes(),
        b"\n",
        second.as_bytes(),
        b"-AFTER\n",
    ]);
    assert_eq!(output, expected);
}

#[test]
fn newline_free_echo_stays_within_the_flush_bound() {
    let payload = vec![b'q'; MAX_PENDING_ECHO_BYTES * 3 + 17];
    let chunkings = [
        1usize,
        8192,
        MAX_PENDING_ECHO_BYTES - 1,
        MAX_PENDING_ECHO_BYTES,
        MAX_PENDING_ECHO_BYTES + 64,
        payload.len(),
    ];
    for chunk in chunkings {
        let output = echo_chunked(&payload, chunk);
        assert_eq!(output.as_bytes(), payload.as_slice(), "chunk {chunk}");
    }
}

#[test]
fn a_partial_line_after_a_newline_stays_within_the_flush_bound() {
    let mut payload = b"head-line\n".to_vec();
    payload.extend(std::iter::repeat_n(b'q', MAX_PENDING_ECHO_BYTES * 2 + 9));
    let output = echo_chunked(&payload, 4096);
    assert_eq!(output.as_bytes(), payload.as_slice());

    let mut echo = RedactingEcho::new(Vec::new());
    echo.push(b"head-line\n");
    echo.push(&vec![b'q'; MAX_PENDING_ECHO_BYTES * 2 + 9]);
    assert!(
        echo.buffered_len() <= MAX_PENDING_ECHO_BYTES,
        "pending {}",
        echo.buffered_len()
    );
    let output = String::from_utf8(echo.finish()).expect("utf8 echo");
    assert!(output.starts_with("head-line\n"));
    assert_eq!(
        output.len(),
        "head-line\n".len() + MAX_PENDING_ECHO_BYTES * 2 + 9
    );
}

#[test]
fn a_multibyte_character_split_by_the_flush_bound_stays_intact() {
    let mut payload = vec![b'a'; MAX_PENDING_ECHO_BYTES - 1];
    payload.extend_from_slice("é".as_bytes());
    payload.extend_from_slice(b"[[tail]]");
    for chunk in [1usize, 8192, MAX_PENDING_ECHO_BYTES, payload.len()] {
        let output = echo_chunked(&payload, chunk);
        assert_eq!(output.as_bytes(), payload.as_slice(), "chunk {chunk}");
    }
}
