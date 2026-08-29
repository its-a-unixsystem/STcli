use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tiktoken_rs::{cl100k_base_singleton, o200k_base_singleton};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum TokenizerId {
    #[serde(rename = "tiktoken:cl100k_base")]
    Cl100kBase,
    #[serde(rename = "tiktoken:o200k_base")]
    O200kBase,
}

impl TokenizerId {
    pub fn count(self, text: &str) -> usize {
        match self {
            Self::Cl100kBase => cl100k_base_singleton()
                .encode_with_special_tokens(text)
                .len(),
            Self::O200kBase => o200k_base_singleton()
                .encode_with_special_tokens(text)
                .len(),
        }
    }
}

impl fmt::Display for TokenizerId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Cl100kBase => "tiktoken:cl100k_base",
            Self::O200kBase => "tiktoken:o200k_base",
        })
    }
}

impl FromStr for TokenizerId {
    type Err = TokenizerError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "tiktoken:cl100k_base" => Ok(Self::Cl100kBase),
            "tiktoken:o200k_base" => Ok(Self::O200kBase),
            _ => Err(TokenizerError::Unsupported(value.to_owned())),
        }
    }
}

#[derive(Debug, Error)]
pub enum TokenizerError {
    #[error("tokenizer '{0}' is unsupported; select tiktoken:cl100k_base or tiktoken:o200k_base")]
    Unsupported(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_tokenizers_count_text() {
        assert!(TokenizerId::Cl100kBase.count("hello world") > 0);
        assert!(TokenizerId::O200kBase.count("hello world") > 0);
    }

    #[test]
    fn unknown_tokenizer_fails_instead_of_guessing() {
        assert!(matches!(
            "unknown".parse::<TokenizerId>(),
            Err(TokenizerError::Unsupported(_))
        ));
    }
}
