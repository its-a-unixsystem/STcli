use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const CLI_SCHEMA_V1: &str = "stcli.cli/v1";

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CliEnvelope<T> {
    pub schema: String,
    pub ok: bool,
    pub command: String,
    pub data: Option<T>,
    pub error: Option<CliError>,
    pub warnings: Vec<CliWarning>,
}

impl<T> CliEnvelope<T> {
    pub fn success(command: impl Into<String>, data: T) -> Self {
        Self {
            schema: CLI_SCHEMA_V1.to_owned(),
            ok: true,
            command: command.into(),
            data: Some(data),
            error: None,
            warnings: Vec::new(),
        }
    }

    pub fn failure(command: impl Into<String>, error: CliError) -> Self {
        Self {
            schema: CLI_SCHEMA_V1.to_owned(),
            ok: false,
            command: command.into(),
            data: None,
            error: Some(error),
            warnings: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CliError {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CliWarning {
    pub code: String,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn success_envelope_uses_public_v1_schema() {
        let envelope = CliEnvelope::success("compat.verify", json!({"passed": 3}));
        assert_eq!(envelope.schema, CLI_SCHEMA_V1);
        assert!(envelope.ok);
        assert!(envelope.error.is_none());
    }

    #[test]
    fn failure_envelope_has_no_data() {
        let envelope = CliEnvelope::<Value>::failure(
            "compat.verify",
            CliError {
                code: "invalid_fixture".to_owned(),
                message: "fixture failed".to_owned(),
                details: None,
            },
        );
        assert!(!envelope.ok);
        assert!(envelope.data.is_none());
    }
}
