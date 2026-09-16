mod chunker;
mod doc_fields;
mod index;
mod task_fields;
mod worker;

#[test]
fn cosine_similarity_unit_and_orthogonal() {
    // identical -> 1.0
    let v = vec![1.0f32, 2.0, 3.0];
    assert!((super::cosine_similarity(&v, &v).unwrap() - 1.0).abs() < 1e-6);

    // orthogonal -> ~0.0
    let left = vec![1.0f32, 0.0];
    let right = vec![0.0f32, 1.0];
    assert!((super::cosine_similarity(&left, &right).unwrap()).abs() < 1e-6);

    // zero denom -> 0.0
    let z = vec![0.0f32, 0.0];
    assert_eq!(super::cosine_similarity(&z, &z).unwrap(), 0.0);
}

#[test]
fn cosine_similarity_length_mismatch() {
    let err = super::cosine_similarity(&[1.0], &[1.0, 2.0]).unwrap_err();
    assert!(err.to_string().contains("length mismatch"));
}

#[test]
fn cosine_similarity_blob_matches_decoded_slice_cosine() {
    let query = [1.0f32, 2.0, 3.0];
    let stored = [3.0f32, 2.0, 1.0];
    let blob = super::encode_f32_blob(&stored);
    let expected = super::cosine_similarity(&query, &stored).unwrap();
    let from_blob = super::cosine_similarity_blob(&query, &blob).unwrap();
    assert_eq!(from_blob, expected);
}

#[test]
fn dot_product_blob_matches_unit_cosine() {
    let query = super::normalize_f32(&[1.0, 2.0, 3.0]);
    let stored = super::normalize_f32(&[3.0, 2.0, 1.0]);
    let blob = super::encode_f32_blob(&stored);
    let cosine = super::cosine_similarity(&query, &stored).unwrap();
    let dot = super::dot_product_blob(&query, &blob).unwrap();
    assert!((cosine - dot).abs() < 1e-6);
}

#[test]
fn decode_f32_blob_rejects_truncated_input() {
    let err = super::decode_f32_blob(&[0, 1, 2]).unwrap_err();
    assert!(err.to_string().contains("invalid embedding blob length"));
}

#[test]
fn normalize_f32_unit_and_zero() {
    let unit = super::normalize_f32(&[3.0, 4.0]);
    assert!((super::l2_norm(&unit) - 1.0).abs() < 1e-6);
    assert_eq!(unit, vec![0.6, 0.8]);
    assert_eq!(super::normalize_f32(&[0.0, 0.0]), vec![0.0, 0.0]);
}
