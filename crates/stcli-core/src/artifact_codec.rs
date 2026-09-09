use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ArtifactCodecInput {
    /// Source bytes encoded as base64.
    pub source: String,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub metadata: serde_json::Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ArtifactCodecAsset {
    pub logical_path: String,
    /// Asset bytes encoded as base64.
    pub bytes: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ArtifactCodecOutput {
    pub kind: String,
    /// Decoded artifact payload encoded as base64.
    pub payload: String,
    #[serde(default)]
    pub assets: Vec<ArtifactCodecAsset>,
    pub format: String,
}
