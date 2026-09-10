use std::io::{Cursor, Read, Write};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use zip::{ZipArchive, ZipWriter, write::SimpleFileOptions};

wit_bindgen::generate!({
    path: "../../wit",
    world: "plugin",
});

const INTERFACE_VERSION: &str = "stcli.artifact-codec/v1";
const MAX_SOURCE_BYTES: usize = 2 * 1024 * 1024;
const MAX_ENTRY_BYTES: usize = 4 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "kebab-case")]
enum CodecInput {
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
        bundle: Bundle,
    },
}

#[derive(Deserialize, Serialize)]
struct Asset {
    logical_path: String,
    bytes: String,
    byte_size: usize,
    sha256: String,
}

#[derive(Deserialize, Serialize)]
struct Bundle {
    artifact_kind: String,
    source_format: String,
    payload: String,
    payload_sha256: String,
    assets: Vec<Asset>,
}

struct Ccv3Codec;

impl Guest for Ccv3Codec {
    fn run(input: String) -> Result<String, String> {
        let input: serde_json::Value = serde_json::from_str(&input).map_err(|e| e.to_string())?;
        let request: CodecInput =
            serde_json::from_value(input["payload"].clone()).map_err(|e| e.to_string())?;
        let value = match request {
            CodecInput::Detect {
                interface_version,
                source,
            } => detect(&interface_version, &source)?,
            CodecInput::Decode {
                interface_version,
                source,
            } => decode(&interface_version, &source)?,
            CodecInput::Encode {
                interface_version,
                format,
                bundle,
            } => encode(&interface_version, &format, bundle)?,
        };
        serde_json::to_string(&json!({
            "effects": [{"effect": "output", "value": value}]
        }))
        .map_err(|e| e.to_string())
    }
}

fn detect(interface_version: &str, source: &str) -> Result<serde_json::Value, String> {
    require_interface(interface_version)?;
    let source = decode_base64(source)?;
    if source.len() > MAX_SOURCE_BYTES {
        return Err("codec source exceeds maximum size".to_owned());
    }
    let compatible = source.starts_with(b"PK\x03\x04");
    Ok(json!({
        "operation": "detect",
        "interface_version": INTERFACE_VERSION,
        "compatible": compatible,
        "compatibility": if compatible {
            json!([{"code": "ccv3-charx", "message": "CCv3 CHARX archive detected."}])
        } else {
            json!([])
        }
    }))
}

fn decode(interface_version: &str, source: &str) -> Result<serde_json::Value, String> {
    require_interface(interface_version)?;
    let source = decode_base64(source)?;
    if source.len() > MAX_SOURCE_BYTES {
        return Err("codec source exceeds maximum size".to_owned());
    }
    let mut archive = ZipArchive::new(Cursor::new(source)).map_err(|e| e.to_string())?;
    let mut payload = Vec::new();
    archive
        .by_name("card.json")
        .map_err(|e| e.to_string())?
        .take((MAX_ENTRY_BYTES + 1) as u64)
        .read_to_end(&mut payload)
        .map_err(|e| e.to_string())?;
    if payload.len() > MAX_ENTRY_BYTES {
        return Err("card.json exceeds maximum size".to_owned());
    }
    let mut assets = Vec::new();
    for index in 0..archive.len() {
        let entry = archive.by_index(index).map_err(|e| e.to_string())?;
        let path = entry.name().to_owned();
        if entry.is_dir() || !path.starts_with("assets/") {
            continue;
        }
        let mut bytes = Vec::new();
        entry
            .take((MAX_ENTRY_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > MAX_ENTRY_BYTES {
            return Err("asset exceeds maximum size".to_owned());
        }
        assets.push(Asset {
            logical_path: path,
            byte_size: bytes.len(),
            sha256: sha256(&bytes),
            bytes: BASE64.encode(bytes),
        });
    }
    assets.sort_by(|left, right| left.logical_path.cmp(&right.logical_path));
    let bundle = Bundle {
        artifact_kind: "character-card-v3".to_owned(),
        source_format: "json".to_owned(),
        payload: BASE64.encode(&payload),
        payload_sha256: sha256(&payload),
        assets,
    };
    Ok(json!({
        "operation": "decode",
        "interface_version": INTERFACE_VERSION,
        "format": "charx",
        "bundle": bundle,
        "compatibility": [{"code": "ccv3-decoded", "message": "CCv3 data and embedded assets decoded."}]
    }))
}

fn encode(
    interface_version: &str,
    format: &str,
    bundle: Bundle,
) -> Result<serde_json::Value, String> {
    require_interface(interface_version)?;
    if format != "charx" || bundle.source_format != "json" {
        return Err(format!("unsupported format '{format}'"));
    }
    let mut archive = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let payload = decode_base64(&bundle.payload)?;
    if payload.len() > MAX_ENTRY_BYTES {
        return Err("card.json exceeds maximum size".to_owned());
    }
    archive
        .start_file("card.json", options)
        .map_err(|e| e.to_string())?;
    archive.write_all(&payload).map_err(|e| e.to_string())?;
    for asset in bundle.assets {
        let bytes = decode_base64(&asset.bytes)?;
        if bytes.len() > MAX_ENTRY_BYTES {
            return Err("asset exceeds maximum size".to_owned());
        }
        archive
            .start_file(asset.logical_path, options)
            .map_err(|e| e.to_string())?;
        archive.write_all(&bytes).map_err(|e| e.to_string())?;
    }
    let source = archive.finish().map_err(|e| e.to_string())?.into_inner();
    if source.len() > MAX_SOURCE_BYTES {
        return Err("encoded source exceeds maximum size".to_owned());
    }
    Ok(json!({
        "operation": "encode",
        "interface_version": INTERFACE_VERSION,
        "source": BASE64.encode(source),
        "compatibility": []
    }))
}

fn require_interface(interface_version: &str) -> Result<(), String> {
    if interface_version == INTERFACE_VERSION {
        Ok(())
    } else {
        Err(format!("unsupported interface '{interface_version}'"))
    }
}

fn decode_base64(value: &str) -> Result<Vec<u8>, String> {
    BASE64.decode(value).map_err(|e| e.to_string())
}

fn sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut value = String::with_capacity(71);
    value.push_str("sha256:");
    for byte in digest {
        use std::fmt::Write as _;
        write!(value, "{byte:02x}").expect("writing to a String cannot fail");
    }
    value
}

export!(Ccv3Codec);
