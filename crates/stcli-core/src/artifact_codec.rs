use std::collections::BTreeSet;

use semver::Version;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{ArtifactKind, ContentHash};

pub const ARTIFACT_CODEC_INTERFACE_VERSION: &str = "stcli.artifact-codec/v1";

pub(crate) const MAX_CODEC_SOURCE_BYTES: usize = 2 * 1024 * 1024;
pub(crate) const MAX_CODEC_PAYLOAD_BYTES: usize = 2 * 1024 * 1024;
pub(crate) const MAX_CODEC_ASSET_BYTES: usize = 4 * 1024 * 1024;
pub(crate) const MAX_CODEC_TOTAL_ASSET_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const MAX_CODEC_ASSETS: usize = 64;
pub(crate) const MAX_CODEC_SUPPLEMENTARY_ARTIFACTS: usize = 64;
pub(crate) const MAX_CODEC_TOTAL_SUPPLEMENTARY_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const MAX_CODEC_COMPATIBILITY_ITEMS: usize = 32;
pub(crate) const MAX_CODEC_COMPATIBILITY_MESSAGE_BYTES: usize = 1024;
pub(crate) const MAX_CODEC_LOGICAL_PATH_BYTES: usize = 512;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "operation", rename_all = "kebab-case")]
pub enum ArtifactCodecInput {
    Detect {
        interface_version: String,
        source: String,
    },
    Decode {
        interface_version: String,
        source: String,
    },
    Encode {
        interface_version: String,
        format: String,
        bundle: ArtifactCodecBundle,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ArtifactCodecAsset {
    pub logical_path: String,
    /// Asset bytes encoded as base64.
    pub bytes: String,
    pub byte_size: usize,
    pub sha256: ContentHash,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ArtifactCodecSupplementary {
    pub logical_path: String,
    pub artifact_kind: ArtifactKind,
    pub source_format: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub embedded: bool,
    /// Decoded supplementary Artifact payload encoded as base64.
    pub payload: String,
    pub byte_size: usize,
    pub payload_sha256: ContentHash,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ArtifactCodecBundle {
    pub artifact_kind: ArtifactKind,
    pub source_format: String,
    /// Decoded Artifact payload encoded as base64.
    pub payload: String,
    pub payload_sha256: ContentHash,
    #[serde(default)]
    pub assets: Vec<ArtifactCodecAsset>,
    #[serde(default)]
    pub supplementary_artifacts: Vec<ArtifactCodecSupplementary>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ArtifactCodecCompatibility {
    pub code: String,
    pub message: String,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ArtifactCodecDeclaration {
    pub interface_versions: BTreeSet<String>,
    pub formats: BTreeSet<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ArtifactCodecSupplementaryProvenance {
    pub logical_path: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub embedded: bool,
    pub revision_hash: ContentHash,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "operation", rename_all = "kebab-case")]
pub enum ArtifactCodecOutput {
    Detect {
        interface_version: String,
        compatible: bool,
        #[serde(default)]
        compatibility: Vec<ArtifactCodecCompatibility>,
    },
    Decode {
        interface_version: String,
        format: String,
        bundle: ArtifactCodecBundle,
        #[serde(default)]
        compatibility: Vec<ArtifactCodecCompatibility>,
    },
    Encode {
        interface_version: String,
        /// Encoded external source bytes as base64.
        source: String,
        #[serde(default)]
        compatibility: Vec<ArtifactCodecCompatibility>,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ArtifactCodecProvenance {
    pub plugin_id: String,
    pub version: Version,
    pub component_sha256: ContentHash,
    pub interface_version: String,
    pub format: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub compatibility: Vec<ArtifactCodecCompatibility>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supplementary_artifacts: Vec<ArtifactCodecSupplementaryProvenance>,
}

#[derive(Debug, Error)]
pub enum ArtifactCodecError {
    #[error("artifact codec bundle failed Artifact validation: {0}")]
    Artifact(#[from] crate::ArtifactError),
    #[error("artifact codec Plugin contract is invalid: {0}")]
    InvalidPluginContract(String),
    #[error("artifact codecs ambiguously claimed the source: {0:?}")]
    AmbiguousFormatClaims(Vec<String>),
    #[error("artifact codec interface version '{actual}' is incompatible; expected '{expected}'")]
    InterfaceVersion { expected: String, actual: String },
    #[error("artifact codec returned '{actual}' for a '{expected}' operation")]
    Operation {
        expected: &'static str,
        actual: &'static str,
    },
    #[error("artifact codec {field} is not valid base64: {source}")]
    InvalidBase64 {
        field: &'static str,
        source: base64::DecodeError,
    },
    #[error("artifact codec {field} hash does not match its bytes")]
    HashMismatch { field: &'static str },
    #[error("artifact codec proposed {proposed} but Core decoded {actual}")]
    ArtifactKindMismatch {
        proposed: ArtifactKind,
        actual: ArtifactKind,
    },
    #[error("artifact codec format '{0}' is invalid")]
    InvalidFormat(String),
    #[error(
        "artifact codec embedded supplementary Artifact claims do not match the primary Artifact"
    )]
    InvalidEmbeddedSupplementary,
    #[error("artifact codec import conflicts with existing Artifact Revision {0}")]
    RevisionConflict(ContentHash),
    #[error("artifact codec proposed {actual} assets; limit is {limit}")]
    AssetCount { actual: usize, limit: usize },
    #[error("artifact codec proposed {actual} supplementary Artifacts; limit is {limit}")]
    SupplementaryArtifactCount { actual: usize, limit: usize },
    #[error("artifact codec payload has {actual} bytes; limit is {limit}")]
    PayloadSize { actual: usize, limit: usize },
    #[error("artifact codec asset '{path}' has {actual} bytes; limit is {limit}")]
    AssetSize {
        path: String,
        actual: usize,
        limit: usize,
    },
    #[error("artifact codec supplementary Artifacts have {actual} total bytes; limit is {limit}")]
    TotalSupplementarySize { actual: usize, limit: usize },
    #[error("artifact codec asset '{path}' declares {proposed} bytes but contains {actual} bytes")]
    ByteSizeMismatch {
        path: String,
        proposed: usize,
        actual: usize,
    },
    #[error("artifact codec assets have {actual} total bytes; limit is {limit}")]
    TotalAssetSize { actual: usize, limit: usize },
    #[error("artifact codec logical path '{0}' is invalid")]
    InvalidLogicalPath(String),
    #[error("artifact codec logical path '{0}' is duplicated")]
    DuplicateLogicalPath(String),
    #[error("artifact codec returned {actual} compatibility items; limit is {limit}")]
    CompatibilityCount { actual: usize, limit: usize },
    #[error("artifact codec compatibility item '{code}' is invalid")]
    InvalidCompatibility { code: String },
    #[error("artifact codec encoded source has {actual} bytes; limit is {limit}")]
    EncodedSourceSize { actual: usize, limit: usize },
}
