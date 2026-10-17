//  Digest text through the public identity API.
// 
//  Every persisted SHA-256 identity is `identity::lower_hex` of the raw digest,
//  so a change of digest crate must keep these strings byte-identical.

#[test]
fn sha256_digests_encode_as_their_published_vectors() {
    // FIPS 180-2 appendix B.1 and B.2, and the empty message.
    let vectors: [(&[u8], &str); 3] = [
        (
            b"abc",
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        ),
        (
            b"",
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        ),
        (
            b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
        ),
    ];
    for (message, expected) in vectors {
        assert_eq!(lower_hex(Sha256::digest(message)), expected);
        let mut streamed = Sha256::new();
        for chunk in message.chunks(7) {
            streamed.update(chunk);
        }
        assert_eq!(lower_hex(streamed.finalize()), expected);
    }
}

#[test]
fn every_byte_keeps_its_leading_zero() {
    assert_eq!(lower_hex([0x00_u8, 0x0f, 0xa0, 0xff]), "000fa0ff");
    assert_eq!(lower_hex([0_u8; 0]), "");
}
