use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct MetadataText {
    pub num_lines: usize,
    // pub num_words: usize,
    // pub num_chars: usize,
    // pub num_whitespace: usize,
}