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
