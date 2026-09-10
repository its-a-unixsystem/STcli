use std::{
    collections::HashSet,
    io::{Cursor, Read, Write},
    path::Path,
};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use crc32fast::Hasher;
use flate2::read::ZlibDecoder;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use zip::{ZipArchive, ZipWriter, write::SimpleFileOptions};

wit_bindgen::generate!({
    path: "../../wit",
    world: "plugin",
});

const INTERFACE_VERSION: &str = "stcli.artifact-codec/v1";
const MAX_SOURCE_BYTES: usize = 2 * 1024 * 1024;
const MAX_ENTRY_BYTES: usize = 4 * 1024 * 1024;
const MAX_ASSETS: usize = 64;
const MAX_SUPPLEMENTARY: usize = 64;
const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";

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
struct Supplementary {
    logical_path: String,
    artifact_kind: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    embedded: bool,
    source_format: String,
    payload: String,
    byte_size: usize,
    payload_sha256: String,
}

#[derive(Deserialize, Serialize)]
struct Bundle {
    artifact_kind: String,
    source_format: String,
    payload: String,
    payload_sha256: String,
    #[serde(default)]
    assets: Vec<Asset>,
    #[serde(default)]
    supplementary_artifacts: Vec<Supplementary>,
}

struct Decoded {
    format: &'static str,
    artifact_kind: &'static str,
    source_format: &'static str,
    payload: Vec<u8>,
    assets: Vec<(String, Vec<u8>)>,
    supplementary: Vec<(String, &'static str, Vec<u8>, bool)>,
}

struct SillyTavernCodec;

impl Guest for SillyTavernCodec {
    fn run(input: String) -> Result<String, String> {
        let input: Value = serde_json::from_str(&input).map_err(|error| error.to_string())?;
        let request: CodecInput =
            serde_json::from_value(input["payload"].clone()).map_err(|error| error.to_string())?;
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
        .map_err(|error| error.to_string())
    }
}

fn detect(interface_version: &str, source: &str) -> Result<Value, String> {
    require_interface(interface_version)?;
    let source = decode_base64(source)?;
    if source.len() > MAX_SOURCE_BYTES {
        return Err("codec source exceeds maximum size".to_owned());
    }
    let format = detect_format(&source);
    Ok(json!({
        "operation": "detect",
        "interface_version": INTERFACE_VERSION,
        "compatible": format.is_some(),
        "compatibility": format.map_or_else(Vec::new, |format| vec![compatibility(
            format!("sillytavern-{format}"),
            format!("SillyTavern {format} Artifact detected."),
        )]),
    }))
}

fn detect_format(source: &[u8]) -> Option<&'static str> {
    if source.starts_with(b"PK\x03\x04") {
        Some("charx")
    } else if source.starts_with(PNG_SIGNATURE) {
        Some(if has_png_chunk(source, b"acTL") {
            "apng"
        } else {
            "png"
        })
    } else if is_webp(source) {
        Some("webp")
    } else if artifact_kind(source).is_some() {
        Some("json")
    } else {
        None
    }
}

fn decode(interface_version: &str, source: &str) -> Result<Value, String> {
    require_interface(interface_version)?;
    let source = decode_base64(source)?;
    if source.len() > MAX_SOURCE_BYTES {
        return Err("codec source exceeds maximum size".to_owned());
    }
    let decoded = decode_source(&source)?;
    let bundle = Bundle {
        artifact_kind: decoded.artifact_kind.to_owned(),
        source_format: decoded.source_format.to_owned(),
        payload_sha256: sha256(&decoded.payload),
        payload: BASE64.encode(&decoded.payload),
        assets: decoded
            .assets
            .into_iter()
            .map(|(logical_path, bytes)| Asset {
                logical_path,
                byte_size: bytes.len(),
                sha256: sha256(&bytes),
                bytes: BASE64.encode(bytes),
            })
            .collect(),
        supplementary_artifacts: decoded
            .supplementary
            .into_iter()
            .map(
                |(logical_path, artifact_kind, payload, embedded)| Supplementary {
                    logical_path,
                    artifact_kind: artifact_kind.to_owned(),
                    embedded,
                    source_format: "json".to_owned(),
                    byte_size: payload.len(),
                    payload_sha256: sha256(&payload),
                    payload: BASE64.encode(payload),
                },
            )
            .collect(),
    };
    Ok(json!({
        "operation": "decode",
        "interface_version": INTERFACE_VERSION,
        "format": decoded.format,
        "bundle": bundle,
        "compatibility": [compatibility(
            format!("sillytavern-{}-decoded", decoded.format),
            format!("SillyTavern {} Artifact decoded.", decoded.format),
        )],
    }))
}

fn encode(interface_version: &str, format: &str, bundle: Bundle) -> Result<Value, String> {
    require_interface(interface_version)?;
    let source = match format {
        "json" => decode_base64(&bundle.payload)?,
        "png" | "apng" => original_image(&bundle.assets, "avatar.png")?,
        "webp" => original_image(&bundle.assets, "avatar.webp")?,
        "charx" => encode_charx(bundle)?,
        _ => return Err(format!("unsupported format '{format}'")),
    };
    if source.len() > MAX_SOURCE_BYTES {
        return Err("encoded source exceeds maximum size".to_owned());
    }
    Ok(json!({
        "operation": "encode",
        "interface_version": INTERFACE_VERSION,
        "source": BASE64.encode(source),
        "compatibility": [],
    }))
}

fn decode_source(source: &[u8]) -> Result<Decoded, String> {
    if source.starts_with(b"PK\x03\x04") {
        return decode_charx(source);
    }
    if source.starts_with(PNG_SIGNATURE) {
        let payload = extract_png_card(source)?;
        let artifact_kind = artifact_kind(&payload).ok_or("unsupported PNG card payload")?;
        return Ok(Decoded {
            format: if has_png_chunk(source, b"acTL") {
                "apng"
            } else {
                "png"
            },
            artifact_kind,
            source_format: "png",
            payload,
            assets: vec![("avatar.png".to_owned(), source.to_vec())],
            supplementary: Vec::new(),
        });
    }
    if is_webp(source) {
        let payload = extract_webp_card(source)?;
        let artifact_kind = artifact_kind(&payload).ok_or("unsupported WebP card payload")?;
        if !matches!(artifact_kind, "character-card-v2" | "character-card-v3") {
            return Err("WebP cards must contain Character Card V2 or V3".to_owned());
        }
        return Ok(Decoded {
            format: "webp",
            artifact_kind,
            source_format: "webp",
            payload,
            assets: vec![("avatar.webp".to_owned(), source.to_vec())],
            supplementary: Vec::new(),
        });
    }
    let artifact_kind = artifact_kind(source).ok_or("unsupported JSON Artifact")?;
    Ok(Decoded {
        format: "json",
        artifact_kind,
        source_format: "json",
        payload: source.to_vec(),
        assets: Vec::new(),
        supplementary: Vec::new(),
    })
}

fn artifact_kind(source: &[u8]) -> Option<&'static str> {
    let Value::Object(object) = serde_json::from_slice(source).ok()? else {
        return None;
    };
    match object.get("spec").and_then(Value::as_str) {
        Some("chara_card_v3") if object.get("data").is_some_and(Value::is_object) => {
            return Some("character-card-v3");
        }
        Some("chara_card_v2") if object.get("data").is_some_and(Value::is_object) => {
            return Some("character-card-v2");
        }
        Some("lorebook_v3")
            if object
                .get("data")
                .and_then(Value::as_object)
                .is_some_and(|data| data.contains_key("entries")) =>
        {
            return Some("lorebook");
        }
        _ => {}
    }
    let v1_fields = [
        "name",
        "description",
        "personality",
        "scenario",
        "first_mes",
        "mes_example",
    ];
    if v1_fields.iter().all(|field| object.contains_key(*field)) {
        Some("character-card-v1")
    } else if object.contains_key("entries") {
        Some("lorebook")
    } else if object.contains_key("prompts") && object.contains_key("prompt_order") {
        Some("chat-completion-preset")
    } else {
        None
    }
}

fn decode_charx(source: &[u8]) -> Result<Decoded, String> {
    let mut archive = ZipArchive::new(Cursor::new(source)).map_err(|error| error.to_string())?;
    let mut seen = HashSet::new();
    let mut payload = None;
    let mut assets = Vec::new();
    let mut supplementary = Vec::new();
    let mut total_assets = 0usize;
    let mut total_supplementary = 0usize;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|error| error.to_string())?;
        let name = std::str::from_utf8(entry.name_raw())
            .map_err(|_| format!("unsafe CHARX path '{}'", entry.name()))?;
        let trimmed = name.strip_suffix('/').unwrap_or(name);
        if trimmed.is_empty()
            || trimmed.starts_with('/')
            || trimmed.contains('\\')
            || trimmed
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
            || entry.enclosed_name().is_none()
        {
            return Err(format!("unsafe CHARX path '{name}'"));
        }
        let logical_path = trimmed.to_owned();
        if !seen.insert(logical_path.clone()) {
            return Err(format!("duplicate CHARX path '{logical_path}'"));
        }
        if entry.is_symlink() || (!entry.is_file() && !entry.is_dir()) {
            return Err(format!("unsupported CHARX entry '{logical_path}'"));
        }
        if entry.encrypted() {
            return Err("encrypted CHARX archives are unsupported".to_owned());
        }
        if entry.is_dir() {
            continue;
        }
        let bytes = read_bounded(&mut entry)?;
        if logical_path == "card.json" {
            payload = Some(bytes);
        } else if Path::new(&logical_path)
            .file_name()
            .and_then(|name| name.to_str())
            == Some("lorebook.json")
        {
            total_supplementary = total_supplementary
                .checked_add(bytes.len())
                .ok_or("CHARX supplementary size overflow")?;
            if total_supplementary > 8 * 1024 * 1024 {
                return Err("CHARX supplementary bytes exceed maximum size".to_owned());
            }
            if supplementary.len() >= MAX_SUPPLEMENTARY {
                return Err("CHARX supplementary Artifact count exceeds maximum".to_owned());
            }
            if artifact_kind(&bytes) != Some("lorebook") {
                return Err(format!("invalid lorebook '{logical_path}'"));
            }
            supplementary.push((logical_path, "lorebook", bytes, false));
        } else if is_media_path(&logical_path) {
            if assets.len() >= MAX_ASSETS {
                return Err("CHARX asset count exceeds maximum".to_owned());
            }
            total_assets = total_assets
                .checked_add(bytes.len())
                .ok_or("CHARX asset size overflow")?;
            if total_assets > 8 * 1024 * 1024 {
                return Err("CHARX asset bytes exceed maximum size".to_owned());
            }
            assets.push((logical_path, bytes));
        }
    }
    let payload = payload.ok_or("CHARX card.json is missing")?;
    if artifact_kind(&payload) != Some("character-card-v3") {
        return Err("CHARX card.json is not Character Card V3".to_owned());
    }
    if let Some(character_book) = serde_json::from_slice::<Value>(&payload)
        .ok()
        .and_then(|value| value.get("data")?.get("character_book").cloned())
    {
        let bytes = serde_json::to_vec(&character_book).map_err(|error| error.to_string())?;
        if artifact_kind(&bytes) != Some("lorebook") {
            return Err("embedded character_book is not a lorebook".to_owned());
        }
        let mut index = 0usize;
        let logical_path = loop {
            let candidate = format!("lorebooks/character_book-{index}/lorebook.json");
            if seen.insert(candidate.clone()) {
                break candidate;
            }
            index = index
                .checked_add(1)
                .ok_or("embedded lorebook path overflow")?;
        };
        supplementary.push((logical_path, "lorebook", bytes, true));
    }
    assets.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(Decoded {
        format: "charx",
        artifact_kind: "character-card-v3",
        source_format: "json",
        payload,
        assets,
        supplementary,
    })
}

fn encode_charx(bundle: Bundle) -> Result<Vec<u8>, String> {
    if bundle.artifact_kind != "character-card-v3" || bundle.source_format != "json" {
        return Err("CHARX encode requires a flat Character Card V3 JSON Artifact".to_owned());
    }
    let mut archive = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    write_archive_file(
        &mut archive,
        "card.json",
        &decode_base64(&bundle.payload)?,
        options,
    )?;
    for supplementary in bundle.supplementary_artifacts {
        if !supplementary.embedded {
            write_archive_file(
                &mut archive,
                &supplementary.logical_path,
                &decode_base64(&supplementary.payload)?,
                options,
            )?;
        }
    }
    for asset in bundle.assets {
        write_archive_file(
            &mut archive,
            &asset.logical_path,
            &decode_base64(&asset.bytes)?,
            options,
        )?;
    }
    archive
        .finish()
        .map(|cursor| cursor.into_inner())
        .map_err(|error| error.to_string())
}

fn write_archive_file(
    archive: &mut ZipWriter<Cursor<Vec<u8>>>,
    path: &str,
    bytes: &[u8],
    options: SimpleFileOptions,
) -> Result<(), String> {
    if bytes.len() > MAX_ENTRY_BYTES {
        return Err(format!("CHARX entry '{path}' exceeds maximum size"));
    }
    archive
        .start_file(path, options)
        .map_err(|error| error.to_string())?;
    archive.write_all(bytes).map_err(|error| error.to_string())
}

fn original_image(assets: &[Asset], logical_path: &str) -> Result<Vec<u8>, String> {
    let asset = assets
        .iter()
        .find(|asset| asset.logical_path == logical_path)
        .ok_or_else(|| format!("codec image asset '{logical_path}' is missing"))?;
    decode_base64(&asset.bytes)
}

fn extract_png_card(source: &[u8]) -> Result<Vec<u8>, String> {
    if !source.starts_with(PNG_SIGNATURE) {
        return Err("invalid PNG signature".to_owned());
    }
    let mut offset = PNG_SIGNATURE.len();
    let mut ccv3_itxt = None;
    let mut chara_itxt = None;
    let mut chara_text = None;
    let mut complete = false;
    while offset < source.len() {
        let header_end = offset.checked_add(8).ok_or("truncated PNG chunk")?;
        if header_end > source.len() {
            return Err("truncated PNG chunk".to_owned());
        }
        let length = read_u32_be(source, offset)? as usize;
        let chunk_end = header_end
            .checked_add(length)
            .and_then(|end| end.checked_add(4))
            .ok_or("truncated PNG chunk")?;
        if chunk_end > source.len() {
            return Err("truncated PNG chunk".to_owned());
        }
        let kind = &source[offset + 4..header_end];
        let data = &source[header_end..header_end + length];
        let expected_crc = read_u32_be(source, header_end + length)?;
        let mut hasher = Hasher::new();
        hasher.update(kind);
        hasher.update(data);
        if hasher.finalize() != expected_crc {
            return Err("PNG chunk CRC mismatch".to_owned());
        }
        match kind {
            b"tEXt" => {
                if let Some((keyword, text)) = split_once_nul(data)
                    && keyword == b"chara"
                    && chara_text.is_none()
                {
                    chara_text = Some(text);
                }
            }
            b"iTXt" => {
                if let Some((keyword, rest)) = split_once_nul(data)
                    && matches!(keyword, b"chara" | b"ccv3")
                {
                    if keyword == b"ccv3" && ccv3_itxt.is_none() {
                        ccv3_itxt = Some(rest);
                    } else if keyword == b"chara" && chara_itxt.is_none() {
                        chara_itxt = Some(rest);
                    }
                }
            }
            b"IEND" if data.is_empty() => {
                complete = true;
                break;
            }
            b"IEND" => return Err("PNG IEND chunk is not empty".to_owned()),
            _ => {}
        }
        offset = chunk_end;
    }
    if !complete {
        return Err("truncated PNG container".to_owned());
    }
    let candidate = if let Some(data) = ccv3_itxt.or(chara_itxt) {
        decode_itxt(data)?
    } else {
        chara_text
            .map(ToOwned::to_owned)
            .ok_or("PNG card metadata is missing")?
    };
    decode_json_payload(&candidate)
}

fn has_png_chunk(source: &[u8], expected: &[u8; 4]) -> bool {
    if !source.starts_with(PNG_SIGNATURE) {
        return false;
    }
    let mut offset = PNG_SIGNATURE.len();
    while let Ok(length) = read_u32_be(source, offset) {
        let length = length as usize;
        let Some(kind_start) = offset.checked_add(4) else {
            return false;
        };
        let Some(data_start) = kind_start.checked_add(4) else {
            return false;
        };
        let Some(data_end) = data_start.checked_add(length) else {
            return false;
        };
        let Some(chunk_end) = data_end.checked_add(4) else {
            return false;
        };
        if chunk_end > source.len() {
            return false;
        }
        if &source[kind_start..data_start] == expected {
            return true;
        }
        offset = chunk_end;
    }
    false
}

fn decode_itxt(data: &[u8]) -> Result<Vec<u8>, String> {
    let (&compressed, rest) = data
        .split_first()
        .ok_or("invalid PNG iTXt compression flag")?;
    let (&compression_method, rest) = rest
        .split_first()
        .ok_or("invalid PNG iTXt compression method")?;
    let (_, rest) = split_once_nul(rest).ok_or("invalid PNG iTXt language")?;
    let (_, text) = split_once_nul(rest).ok_or("invalid PNG iTXt translated keyword")?;
    match (compressed, compression_method) {
        (0, 0) => Ok(text.to_vec()),
        (1, 0) => read_zlib(text),
        (0 | 1, _) => Err("unsupported PNG iTXt compression method".to_owned()),
        _ => Err("invalid PNG iTXt compression flag".to_owned()),
    }
}

fn is_webp(source: &[u8]) -> bool {
    source.len() >= 12 && &source[..4] == b"RIFF" && &source[8..12] == b"WEBP"
}

fn extract_webp_card(source: &[u8]) -> Result<Vec<u8>, String> {
    if !is_webp(source) {
        return Err("invalid RIFF WebP signature".to_owned());
    }
    let declared =
        u32::from_le_bytes(source[4..8].try_into().map_err(|_| "invalid WebP size")?) as usize;
    if declared.checked_add(8) != Some(source.len()) {
        return Err("WebP RIFF size does not match input length".to_owned());
    }
    let mut offset = 12usize;
    let mut exif = None;
    let mut xmp = None;
    while offset < source.len() {
        let header_end = offset.checked_add(8).ok_or("WebP chunk header overflow")?;
        if header_end > source.len() {
            return Err("truncated WebP chunk header".to_owned());
        }
        let kind = &source[offset..offset + 4];
        let length = u32::from_le_bytes(
            source[offset + 4..header_end]
                .try_into()
                .map_err(|_| "invalid WebP chunk size")?,
        ) as usize;
        let data_end = header_end
            .checked_add(length)
            .ok_or("WebP chunk length overflow")?;
        let chunk_end = data_end
            .checked_add(length % 2)
            .ok_or("WebP chunk padding overflow")?;
        if chunk_end > source.len() {
            return Err("truncated WebP chunk".to_owned());
        }
        if length % 2 == 1 && source[data_end] != 0 {
            return Err("non-zero WebP chunk padding byte".to_owned());
        }
        if kind == b"EXIF" && exif.is_none() {
            exif = Some(&source[header_end..data_end]);
        } else if kind == b"XMP " && xmp.is_none() {
            xmp = Some(&source[header_end..data_end]);
        }
        offset = chunk_end;
    }
    if let Some(exif) = exif
        && let Some(comment) = extract_exif_user_comment(exif)?
    {
        return decode_json_payload(&comment);
    }
    if let Some(xmp) = xmp
        && let Some(description) = extract_xmp_description(xmp)?
    {
        return decode_json_payload(&description);
    }
    Err("WebP card metadata is missing".to_owned())
}

#[derive(Clone, Copy)]
enum ByteOrder {
    Little,
    Big,
}

impl ByteOrder {
    fn u16(self, bytes: &[u8]) -> Result<u16, String> {
        let bytes: [u8; 2] = bytes.try_into().map_err(|_| "truncated TIFF u16")?;
        Ok(match self {
            Self::Little => u16::from_le_bytes(bytes),
            Self::Big => u16::from_be_bytes(bytes),
        })
    }

    fn u32(self, bytes: &[u8]) -> Result<u32, String> {
        let bytes: [u8; 4] = bytes.try_into().map_err(|_| "truncated TIFF u32")?;
        Ok(match self {
            Self::Little => u32::from_le_bytes(bytes),
            Self::Big => u32::from_be_bytes(bytes),
        })
    }
}

fn extract_exif_user_comment(exif: &[u8]) -> Result<Option<Vec<u8>>, String> {
    let tiff = exif.strip_prefix(b"Exif\0\0").unwrap_or(exif);
    if tiff.len() < 8 {
        return Err("truncated TIFF header".to_owned());
    }
    let order = match &tiff[..2] {
        b"II" => ByteOrder::Little,
        b"MM" => ByteOrder::Big,
        _ => return Err("invalid TIFF byte order".to_owned()),
    };
    if order.u16(&tiff[2..4])? != 42 {
        return Err("invalid TIFF signature".to_owned());
    }
    let first_ifd = order.u32(&tiff[4..8])? as usize;
    let (comment, exif_ifd) = read_ifd(tiff, first_ifd, order)?;
    if comment.is_some() {
        return comment
            .map(|value| decode_user_comment(&value, order))
            .transpose();
    }
    if let Some(offset) = exif_ifd {
        let (comment, _) = read_ifd(tiff, offset, order)?;
        return comment
            .map(|value| decode_user_comment(&value, order))
            .transpose();
    }
    Ok(None)
}

fn read_ifd(
    tiff: &[u8],
    offset: usize,
    order: ByteOrder,
) -> Result<(Option<Vec<u8>>, Option<usize>), String> {
    let count_end = offset.checked_add(2).ok_or("TIFF offset overflow")?;
    if count_end > tiff.len() {
        return Err("truncated TIFF IFD".to_owned());
    }
    let count = order.u16(&tiff[offset..count_end])? as usize;
    let mut comment = None;
    let mut exif_ifd = None;
    for index in 0..count {
        let start = count_end
            .checked_add(index.checked_mul(12).ok_or("TIFF entry overflow")?)
            .ok_or("TIFF entry overflow")?;
        let end = start.checked_add(12).ok_or("TIFF entry overflow")?;
        if end > tiff.len() {
            return Err("truncated TIFF entry".to_owned());
        }
        let entry = &tiff[start..end];
        let tag = order.u16(&entry[..2])?;
        if tag == 0x9286 {
            comment = Some(read_tiff_field(tiff, entry, order)?);
        } else if tag == 0x8769 {
            exif_ifd = Some(order.u32(&entry[8..12])? as usize);
        }
    }
    Ok((comment, exif_ifd))
}

fn read_tiff_field(tiff: &[u8], entry: &[u8], order: ByteOrder) -> Result<Vec<u8>, String> {
    let field_type = order.u16(&entry[2..4])?;
    let count = order.u32(&entry[4..8])? as usize;
    let width = match field_type {
        1 | 2 | 7 => 1,
        3 => 2,
        4 | 9 => 4,
        5 | 10 => 8,
        _ => return Err("unsupported TIFF field type".to_owned()),
    };
    let length = count
        .checked_mul(width)
        .ok_or("TIFF field length overflow")?;
    if length <= 4 {
        return Ok(entry[8..8 + length].to_vec());
    }
    let offset = order.u32(&entry[8..12])? as usize;
    let end = offset
        .checked_add(length)
        .ok_or("TIFF field length overflow")?;
    tiff.get(offset..end)
        .map(ToOwned::to_owned)
        .ok_or_else(|| "truncated TIFF field".to_owned())
}

fn decode_user_comment(comment: &[u8], order: ByteOrder) -> Result<Vec<u8>, String> {
    if let Some(body) = comment.strip_prefix(b"ASCII\0\0\0") {
        return Ok(trim_ascii(body).to_vec());
    }
    if let Some(body) = comment.strip_prefix(b"UNICODE\0") {
        let mut units = Vec::with_capacity(body.len() / 2);
        for chunk in body.chunks_exact(2) {
            units.push(order.u16(chunk)?);
        }
        return String::from_utf16(&units)
            .map(|value| value.trim_matches('\0').as_bytes().to_vec())
            .map_err(|error| error.to_string());
    }
    Ok(trim_ascii(comment.strip_prefix(&[0; 8]).unwrap_or(comment)).to_vec())
}

struct XmlElement<'a> {
    content: &'a [u8],
    empty: bool,
}

fn extract_xmp_description(xmp: &[u8]) -> Result<Option<Vec<u8>>, String> {
    let Some(description) = find_xml_element(xmp, &[b"dc:description", b"xmp:description"])? else {
        return Ok(None);
    };
    if description.empty {
        return Err("empty WebP description".to_owned());
    }
    let content = match find_xml_element(description.content, &[b"rdf:li"])? {
        Some(item) if !item.empty => item.content,
        Some(_) => return Err("empty WebP description".to_owned()),
        None => description.content,
    };
    let decoded = xml_text_content(content)?;
    let decoded = trim_ascii(&decoded);
    if decoded.is_empty() {
        return Err("empty WebP description".to_owned());
    }
    Ok(Some(decoded.to_vec()))
}

fn find_xml_element<'a>(xml: &'a [u8], names: &[&[u8]]) -> Result<Option<XmlElement<'a>>, String> {
    let mut offset = 0;
    while let Some(relative) = xml[offset..].iter().position(|byte| *byte == b'<') {
        let start = offset + relative;
        if xml[start..].starts_with(b"<![CDATA[") {
            let end =
                find_bytes(&xml[start + 9..], b"]]>").ok_or("unterminated XML CDATA section")?;
            offset = start + 9 + end + 3;
            continue;
        }
        if xml[start..].starts_with(b"<!--") {
            let end = find_bytes(&xml[start + 4..], b"-->").ok_or("unterminated XML comment")?;
            offset = start + 4 + end + 3;
            continue;
        }
        let name_start = start + 1;
        for name in names {
            let Some(after_name) = name_start.checked_add(name.len()) else {
                continue;
            };
            if xml.get(name_start..after_name) != Some(*name)
                || !xml
                    .get(after_name)
                    .is_some_and(|byte| byte.is_ascii_whitespace() || matches!(*byte, b'/' | b'>'))
            {
                continue;
            }
            let opening_end = find_xml_tag_end(xml, after_name)?;
            let empty = trim_ascii(&xml[after_name..opening_end]).ends_with(b"/");
            if empty {
                return Ok(Some(XmlElement {
                    content: &[],
                    empty: true,
                }));
            }
            let content_start = opening_end + 1;
            let content_end = find_xml_close(xml, content_start, name)?;
            return Ok(Some(XmlElement {
                content: &xml[content_start..content_end],
                empty: false,
            }));
        }
        offset = name_start;
    }
    Ok(None)
}

fn find_xml_tag_end(xml: &[u8], start: usize) -> Result<usize, String> {
    let mut quote = None;
    for (relative, byte) in xml[start..].iter().copied().enumerate() {
        match (quote, byte) {
            (Some(active), current) if active == current => quote = None,
            (None, b'\'' | b'"') => quote = Some(byte),
            (None, b'>') => return Ok(start + relative),
            _ => {}
        }
    }
    Err("unterminated XML tag".to_owned())
}

fn find_xml_close(xml: &[u8], mut offset: usize, name: &[u8]) -> Result<usize, String> {
    while let Some(relative) = xml[offset..].iter().position(|byte| *byte == b'<') {
        let start = offset + relative;
        if xml[start..].starts_with(b"<![CDATA[") {
            let end =
                find_bytes(&xml[start + 9..], b"]]>").ok_or("unterminated XML CDATA section")?;
            offset = start + 9 + end + 3;
            continue;
        }
        if xml[start..].starts_with(b"<!--") {
            let end = find_bytes(&xml[start + 4..], b"-->").ok_or("unterminated XML comment")?;
            offset = start + 4 + end + 3;
            continue;
        }
        let name_start = start + 2;
        let Some(after_name) = name_start.checked_add(name.len()) else {
            break;
        };
        if xml.get(start + 1) == Some(&b'/')
            && xml.get(name_start..after_name) == Some(name)
            && let Some(relative_end) = xml[after_name..].iter().position(|byte| *byte == b'>')
            && trim_ascii(&xml[after_name..after_name + relative_end]).is_empty()
        {
            return Ok(start);
        }
        offset = start + 1;
    }
    Err("WebP description element is not closed".to_owned())
}

fn xml_text_content(xml: &[u8]) -> Result<Vec<u8>, String> {
    let mut decoded = Vec::with_capacity(xml.len());
    let mut offset = 0;
    while offset < xml.len() {
        let Some(relative) = xml[offset..].iter().position(|byte| *byte == b'<') else {
            append_unescaped_xml(&mut decoded, &xml[offset..])?;
            break;
        };
        let start = offset + relative;
        append_unescaped_xml(&mut decoded, &xml[offset..start])?;
        if xml[start..].starts_with(b"<![CDATA[") {
            let content_start = start + 9;
            let length = find_bytes(&xml[content_start..], b"]]>")
                .ok_or("unterminated XML CDATA section")?;
            decoded.extend_from_slice(&xml[content_start..content_start + length]);
            offset = content_start + length + 3;
        } else if xml[start..].starts_with(b"<!--") {
            let length = find_bytes(&xml[start + 4..], b"-->").ok_or("unterminated XML comment")?;
            offset = start + 4 + length + 3;
        } else {
            offset = find_xml_tag_end(xml, start + 1)? + 1;
        }
    }
    Ok(decoded)
}

fn append_unescaped_xml(output: &mut Vec<u8>, text: &[u8]) -> Result<(), String> {
    let text = std::str::from_utf8(text).map_err(|_| "XMP text is not UTF-8")?;
    let mut offset = 0;
    while let Some(relative) = text[offset..].find('&') {
        let start = offset + relative;
        output.extend_from_slice(&text.as_bytes()[offset..start]);
        let end = text[start + 1..]
            .find(';')
            .map(|relative| start + 1 + relative)
            .ok_or("unterminated XML entity")?;
        let entity = &text[start + 1..end];
        let character = match entity {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "quot" => '"',
            "apos" => '\'',
            value if value.starts_with("#x") => u32::from_str_radix(&value[2..], 16)
                .ok()
                .and_then(char::from_u32)
                .ok_or("invalid XML entity")?,
            value if value.starts_with('#') => value[1..]
                .parse::<u32>()
                .ok()
                .and_then(char::from_u32)
                .ok_or("invalid XML entity")?,
            _ => return Err("unknown XML entity".to_owned()),
        };
        let mut buffer = [0; 4];
        output.extend_from_slice(character.encode_utf8(&mut buffer).as_bytes());
        offset = end + 1;
    }
    output.extend_from_slice(&text.as_bytes()[offset..]);
    Ok(())
}

fn decode_json_payload(payload: &[u8]) -> Result<Vec<u8>, String> {
    let payload = trim_ascii(payload);
    if serde_json::from_slice::<Value>(payload).is_ok() {
        return Ok(payload.to_vec());
    }
    let decoded = BASE64.decode(payload).map_err(|error| error.to_string())?;
    if serde_json::from_slice::<Value>(&decoded).is_err() {
        return Err("card metadata is not JSON".to_owned());
    }
    Ok(decoded)
}

fn read_bounded(reader: &mut impl Read) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    reader
        .take((MAX_ENTRY_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > MAX_ENTRY_BYTES {
        return Err("CHARX entry exceeds maximum size".to_owned());
    }
    Ok(bytes)
}

fn read_zlib(source: &[u8]) -> Result<Vec<u8>, String> {
    let mut output = Vec::new();
    ZlibDecoder::new(source)
        .take((MAX_ENTRY_BYTES + 1) as u64)
        .read_to_end(&mut output)
        .map_err(|error| error.to_string())?;
    if output.len() > MAX_ENTRY_BYTES {
        return Err("decompressed metadata exceeds maximum size".to_owned());
    }
    Ok(output)
}

fn read_u32_be(source: &[u8], offset: usize) -> Result<u32, String> {
    let end = offset.checked_add(4).ok_or("integer offset overflow")?;
    let bytes: [u8; 4] = source
        .get(offset..end)
        .ok_or("truncated integer")?
        .try_into()
        .map_err(|_| "truncated integer")?;
    Ok(u32::from_be_bytes(bytes))
}

fn is_media_path(path: &str) -> bool {
    path.rsplit_once('.')
        .map(|(_, extension)| extension)
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "png" | "apng" | "webp" | "jpg" | "jpeg" | "gif" | "avif" | "wav" | "mp3" | "ogg"
            )
        })
}

fn split_once_nul(value: &[u8]) -> Option<(&[u8], &[u8])> {
    let index = value.iter().position(|byte| *byte == 0)?;
    Some((&value[..index], &value[index + 1..]))
}

fn trim_ascii(mut value: &[u8]) -> &[u8] {
    while value.first().is_some_and(u8::is_ascii_whitespace) {
        value = &value[1..];
    }
    while value.last().is_some_and(u8::is_ascii_whitespace) {
        value = &value[..value.len() - 1];
    }
    value
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn compatibility(code: String, message: String) -> Value {
    json!({"code": code, "message": message})
}

fn require_interface(interface_version: &str) -> Result<(), String> {
    if interface_version == INTERFACE_VERSION {
        Ok(())
    } else {
        Err(format!("unsupported interface '{interface_version}'"))
    }
}

fn decode_base64(value: &str) -> Result<Vec<u8>, String> {
    BASE64.decode(value).map_err(|error| error.to_string())
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

export!(SillyTavernCodec);
