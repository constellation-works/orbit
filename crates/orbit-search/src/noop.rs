//! Deterministic, normalized hash-based embedder used by tests so they never
//! need a real companion subprocess. The vector is derived from FNV-1a over
//! the input bytes plus an xorshift expansion to fill `dim` floats.

use orbit_common::OrbitError;

use crate::Embedder;

#[derive(Debug, Clone)]
pub struct NoopEmbedder {
    model_id: String,
    dim: usize,
    max_input_tokens: usize,
}

impl NoopEmbedder {
    pub fn new(model_id: impl Into<String>, dim: usize, max_input_tokens: usize) -> Self {
        Self {
            model_id: model_id.into(),
            dim,
            max_input_tokens,
        }
    }

    pub fn small() -> Self {
        Self::new("noop", 3, 512)
    }
}

impl Default for NoopEmbedder {
    fn default() -> Self {
        Self::small()
    }
}

impl Embedder for NoopEmbedder {
    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn dim(&self) -> usize {
        self.dim
    }

    fn max_input_tokens(&self) -> usize {
        self.max_input_tokens
    }

    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, OrbitError> {
        Ok(texts
            .iter()
            .map(|text| noop_vector(text, self.dim))
            .collect())
    }

    fn token_count(&self, text: &str) -> Result<usize, OrbitError> {
        Ok(text.split_whitespace().count().max(1))
    }

    fn token_boundaries(&self, text: &str) -> Result<Vec<usize>, OrbitError> {
        Ok(word_ends(text))
    }
}

fn word_ends(text: &str) -> Vec<usize> {
    let mut ends = Vec::new();
    let mut in_word = false;

    for (index, character) in text.char_indices() {
        if character.is_whitespace() {
            if in_word {
                ends.push(index);
                in_word = false;
            }
        } else {
            in_word = true;
        }
    }
    if in_word {
        ends.push(text.len());
    }
    ends
}

fn noop_vector(text: &str, dim: usize) -> Vec<f32> {
    let mut state = 0xcbf29ce484222325_u64;
    for byte in text.as_bytes() {
        state ^= u64::from(*byte);
        state = state.wrapping_mul(0x100000001b3);
    }

    let mut values = Vec::with_capacity(dim);
    let mut next = state;
    for _ in 0..dim {
        next ^= next << 13;
        next ^= next >> 7;
        next ^= next << 17;
        let scaled = (next as f64 / u64::MAX as f64) as f32;
        values.push((scaled * 2.0) - 1.0);
    }
    normalize(values)
}

fn normalize(mut values: Vec<f32>) -> Vec<f32> {
    let norm = values.iter().map(|value| value * value).sum::<f32>().sqrt();
    if norm > 0.0 {
        for value in &mut values {
            *value /= norm;
        }
    }
    values
}
