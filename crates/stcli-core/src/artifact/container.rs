use std::{
    collections::HashSet,
    io::{Cursor, Read},
    str,
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use crc32fast::Hasher;
use flate2::read::ZlibDecoder;

use super::ArtifactError;

pub(super) const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
const CHARX_SIGNATURE: &[u8; 4] = b"PK\x03\x04";

pub(super) struct CharxArchive {
    pub card_json: Vec<u8>,
    pub lorebooks: Vec<Vec<u8>>,
    pub assets: Vec<CharxAsset>,
}

pub(super) struct CharxAsset {
    pub logical_path: String,
    pub bytes: Vec<u8>,
}

pub(super) fn is_charx(source: &[u8]) -> bool {
    source.starts_with(CHARX_SIGNATURE)
}

pub(super) fn is_webp(source: &[u8]) -> bool {
    source.len() >= 12 && &source[..4] == b"RIFF" && &source[8..12] == b"WEBP"
}

pub(super) fn extract_webp_card(source: &[u8]) -> Result<Vec<u8>, ArtifactError> {
    if !is_webp(source) {
        return Err(ArtifactError::InvalidWebp("invalid RIFF WebP signature"));
    }
    let riff_size = u32::from_le_bytes(
        source[4..8]
            .try_into()
            .expect("RIFF size contains four bytes"),
    ) as usize;
    if riff_size.checked_add(8) != Some(source.len()) {
        return Err(ArtifactError::InvalidWebp(
            "RIFF size does not match input length",
        ));
    }

    let mut offset = 12;
    let mut exif = None;
    let mut xmp = None;
    while offset < source.len() {
        let header_end = offset
            .checked_add(8)
            .ok_or(ArtifactError::InvalidWebp("chunk header offset overflow"))?;
        if header_end > source.len() {
            return Err(ArtifactError::InvalidWebp("truncated chunk header"));
        }
        let length = u32::from_le_bytes(
            source[offset + 4..header_end]
                .try_into()
                .expect("WebP chunk size contains four bytes"),
        ) as usize;
        let data_end = header_end
            .checked_add(length)
            .ok_or(ArtifactError::InvalidWebp("chunk size overflow"))?;
        let chunk_end = data_end
            .checked_add(length % 2)
            .ok_or(ArtifactError::InvalidWebp("chunk padding overflow"))?;
        if chunk_end > source.len() {
            return Err(ArtifactError::InvalidWebp("truncated chunk data"));
        }
        if length % 2 == 1 && source[data_end] != 0 {
            return Err(ArtifactError::InvalidWebp("non-zero chunk padding byte"));
        }

        let data = &source[header_end..data_end];
        match &source[offset..offset + 4] {
            b"EXIF" if exif.is_none() => exif = Some(data),
            b"XMP " if xmp.is_none() => xmp = Some(data),
            _ => {}
        }
        offset = chunk_end;
    }

    if let Some(exif) = exif
        && let Some(payload) = extract_exif_user_comment(exif)?
    {
        return decode_webp_json_payload(&payload);
    }
    if let Some(xmp) = xmp
        && let Some(payload) = extract_xmp_description(xmp)?
    {
        return decode_webp_json_payload(&payload);
    }
    Err(ArtifactError::MissingWebpMetadata)
}

#[derive(Clone, Copy)]
enum TiffByteOrder {
    Little,
    Big,
}

impl TiffByteOrder {
    fn u16(self, bytes: &[u8]) -> u16 {
        let bytes = bytes.try_into().expect("TIFF u16 contains two bytes");
        match self {
            Self::Little => u16::from_le_bytes(bytes),
            Self::Big => u16::from_be_bytes(bytes),
        }
    }

    fn u32(self, bytes: &[u8]) -> u32 {
        let bytes = bytes.try_into().expect("TIFF u32 contains four bytes");
        match self {
            Self::Little => u32::from_le_bytes(bytes),
            Self::Big => u32::from_be_bytes(bytes),
        }
    }
}

fn extract_exif_user_comment(exif: &[u8]) -> Result<Option<Vec<u8>>, ArtifactError> {
    let tiff = exif.strip_prefix(b"Exif\0\0").unwrap_or(exif);
    if tiff.len() < 8 {
        return Err(ArtifactError::InvalidWebpExif("truncated TIFF header"));
    }
    let byte_order = match &tiff[..2] {
        b"II" => TiffByteOrder::Little,
        b"MM" => TiffByteOrder::Big,
        _ => return Err(ArtifactError::InvalidWebpExif("invalid TIFF byte order")),
    };
    if byte_order.u16(&tiff[2..4]) != 42 {
        return Err(ArtifactError::InvalidWebpExif("invalid TIFF marker"));
    }

    let first_ifd = byte_order.u32(&tiff[4..8]) as usize;
    let (comment, exif_ifd) = read_exif_ifd(tiff, first_ifd, byte_order)?;
    if let Some(comment) = comment {
        return decode_exif_user_comment(&comment, byte_order).map(Some);
    }
    let Some(exif_ifd) = exif_ifd else {
        return Ok(None);
    };
    let (comment, _) = read_exif_ifd(tiff, exif_ifd, byte_order)?;
    comment
        .map(|comment| decode_exif_user_comment(&comment, byte_order))
        .transpose()
}

fn read_exif_ifd(
    tiff: &[u8],
    offset: usize,
    byte_order: TiffByteOrder,
) -> Result<(Option<Vec<u8>>, Option<usize>), ArtifactError> {
    let count_end = offset
        .checked_add(2)
        .ok_or(ArtifactError::InvalidWebpExif("IFD offset overflow"))?;
    if count_end > tiff.len() {
        return Err(ArtifactError::InvalidWebpExif("truncated IFD"));
    }
    let count = byte_order.u16(&tiff[offset..count_end]) as usize;
    let entries_end = count
        .checked_mul(12)
        .and_then(|size| count_end.checked_add(size))
        .ok_or(ArtifactError::InvalidWebpExif("IFD size overflow"))?;
    let ifd_end = entries_end
        .checked_add(4)
        .ok_or(ArtifactError::InvalidWebpExif("IFD size overflow"))?;
    if ifd_end > tiff.len() {
        return Err(ArtifactError::InvalidWebpExif("truncated IFD entries"));
    }

    let mut comment = None;
    let mut exif_ifd = None;
    for entry in tiff[count_end..entries_end].chunks_exact(12) {
        let tag = byte_order.u16(&entry[..2]);
        let field_type = byte_order.u16(&entry[2..4]);
        let field_count = byte_order.u32(&entry[4..8]) as usize;
        match tag {
            0x8769 => {
                if field_type != 4 || field_count != 1 {
                    return Err(ArtifactError::InvalidWebpExif("invalid ExifIFD pointer"));
                }
                exif_ifd = Some(byte_order.u32(&entry[8..12]) as usize);
            }
            0x9286 if comment.is_none() => {
                if !matches!(field_type, 2 | 7) {
                    return Err(ArtifactError::InvalidWebpExif(
                        "unsupported UserComment field type",
                    ));
                }
                comment = Some(read_tiff_field(tiff, entry, field_count, byte_order)?.to_vec());
            }
            _ => {}
        }
    }
    Ok((comment, exif_ifd))
}

fn read_tiff_field<'a>(
    tiff: &'a [u8],
    entry: &'a [u8],
    length: usize,
    byte_order: TiffByteOrder,
) -> Result<&'a [u8], ArtifactError> {
    if length <= 4 {
        return Ok(&entry[8..8 + length]);
    }
    let offset = byte_order.u32(&entry[8..12]) as usize;
    let end = offset
        .checked_add(length)
        .ok_or(ArtifactError::InvalidWebpExif(
            "UserComment offset overflow",
        ))?;
    tiff.get(offset..end)
        .ok_or(ArtifactError::InvalidWebpExif("truncated UserComment"))
}

fn decode_exif_user_comment(
    comment: &[u8],
    byte_order: TiffByteOrder,
) -> Result<Vec<u8>, ArtifactError> {
    if comment.starts_with(b"UNICODE\0") {
        let encoded = &comment[8..];
        if encoded.len() % 2 != 0 {
            return Err(ArtifactError::InvalidWebpExif(
                "odd-length Unicode UserComment",
            ));
        }
        let units = encoded
            .chunks_exact(2)
            .map(|bytes| byte_order.u16(bytes))
            .collect::<Vec<_>>();
        let decoded = String::from_utf16(&units)
            .map_err(|_| ArtifactError::InvalidWebpExif("invalid Unicode UserComment"))?;
        return Ok(decoded
            .trim_matches(['\0', ' ', '\t', '\r', '\n'])
            .as_bytes()
            .to_vec());
    }
    if comment.starts_with(b"JIS\0\0\0\0\0") {
        return Err(ArtifactError::InvalidWebpExif(
            "JIS UserComment encoding is unsupported",
        ));
    }

    let mut payload = if comment.starts_with(b"ASCII\0\0\0")
        || comment
            .get(..8)
            .is_some_and(|prefix| prefix.iter().all(|byte| *byte == 0))
    {
        &comment[8..]
    } else {
        comment
    };
    while payload.last() == Some(&0) {
        payload = &payload[..payload.len() - 1];
    }
    Ok(trim_ascii(payload).to_vec())
}

struct XmlElement<'a> {
    content: &'a [u8],
    empty: bool,
}

fn extract_xmp_description(xmp: &[u8]) -> Result<Option<Vec<u8>>, ArtifactError> {
    let Some(description) = find_xml_element(xmp, &[b"dc:description", b"xmp:description"])? else {
        return Ok(None);
    };
    if description.empty {
        return Err(ArtifactError::EmptyWebpDescription);
    }
    let content = match find_xml_element(description.content, &[b"rdf:li"])? {
        Some(item) if !item.empty => item.content,
        Some(_) => return Err(ArtifactError::EmptyWebpDescription),
        None => description.content,
    };
    let decoded = xml_text_content(content)?;
    let decoded = trim_ascii(&decoded);
    if decoded.is_empty() {
        return Err(ArtifactError::EmptyWebpDescription);
    }
    Ok(Some(decoded.to_vec()))
}

fn find_xml_element<'a>(
    xml: &'a [u8],
    names: &[&[u8]],
) -> Result<Option<XmlElement<'a>>, ArtifactError> {
    let mut offset = 0;
    while let Some(relative) = xml[offset..].iter().position(|byte| *byte == b'<') {
        let start = offset + relative;
        if xml[start..].starts_with(b"<![CDATA[") {
            let end = find_bytes(&xml[start + 9..], b"]]>")
                .ok_or(ArtifactError::InvalidWebpXmp("unterminated CDATA section"))?;
            offset = start + 9 + end + 3;
            continue;
        }
        if xml[start..].starts_with(b"<!--") {
            let end = find_bytes(&xml[start + 4..], b"-->")
                .ok_or(ArtifactError::InvalidWebpXmp("unterminated XML comment"))?;
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

fn find_xml_tag_end(xml: &[u8], start: usize) -> Result<usize, ArtifactError> {
    let mut quote = None;
    for (relative, byte) in xml[start..].iter().copied().enumerate() {
        match (quote, byte) {
            (Some(active), current) if active == current => quote = None,
            (None, b'\'' | b'"') => quote = Some(byte),
            (None, b'>') => return Ok(start + relative),
            _ => {}
        }
    }
    Err(ArtifactError::InvalidWebpXmp("unterminated XML tag"))
}

fn find_xml_close(xml: &[u8], mut offset: usize, name: &[u8]) -> Result<usize, ArtifactError> {
    while let Some(relative) = xml[offset..].iter().position(|byte| *byte == b'<') {
        let start = offset + relative;
        if xml[start..].starts_with(b"<![CDATA[") {
            let end = find_bytes(&xml[start + 9..], b"]]>")
                .ok_or(ArtifactError::InvalidWebpXmp("unterminated CDATA section"))?;
            offset = start + 9 + end + 3;
            continue;
        }
        if xml[start..].starts_with(b"<!--") {
            let end = find_bytes(&xml[start + 4..], b"-->")
                .ok_or(ArtifactError::InvalidWebpXmp("unterminated XML comment"))?;
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
    Err(ArtifactError::InvalidWebpXmp(
        "description element is not closed",
    ))
}

fn xml_text_content(xml: &[u8]) -> Result<Vec<u8>, ArtifactError> {
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
                .ok_or(ArtifactError::InvalidWebpXmp("unterminated CDATA section"))?;
            decoded.extend_from_slice(&xml[content_start..content_start + length]);
            offset = content_start + length + 3;
        } else if xml[start..].starts_with(b"<!--") {
            let length = find_bytes(&xml[start + 4..], b"-->")
                .ok_or(ArtifactError::InvalidWebpXmp("unterminated XML comment"))?;
            offset = start + 4 + length + 3;
        } else {
            offset = find_xml_tag_end(xml, start + 1)? + 1;
        }
    }
    Ok(decoded)
}

fn append_unescaped_xml(output: &mut Vec<u8>, text: &[u8]) -> Result<(), ArtifactError> {
    let text =
        str::from_utf8(text).map_err(|_| ArtifactError::InvalidWebpXmp("XMP text is not UTF-8"))?;
    let mut offset = 0;
    while let Some(relative) = text[offset..].find('&') {
        let start = offset + relative;
        output.extend_from_slice(&text.as_bytes()[offset..start]);
        let end = text[start + 1..]
            .find(';')
            .map(|relative| start + 1 + relative)
            .ok_or(ArtifactError::InvalidWebpXmp("unterminated XML entity"))?;
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
                .ok_or(ArtifactError::InvalidWebpXmp("invalid XML entity"))?,
            value if value.starts_with('#') => value[1..]
                .parse::<u32>()
                .ok()
                .and_then(char::from_u32)
                .ok_or(ArtifactError::InvalidWebpXmp("invalid XML entity"))?,
            _ => return Err(ArtifactError::InvalidWebpXmp("unknown XML entity")),
        };
        let mut buffer = [0; 4];
        output.extend_from_slice(character.encode_utf8(&mut buffer).as_bytes());
        offset = end + 1;
    }
    output.extend_from_slice(&text.as_bytes()[offset..]);
    Ok(())
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn decode_webp_json_payload(payload: &[u8]) -> Result<Vec<u8>, ArtifactError> {
    let payload = trim_ascii(payload);
    if payload.is_empty() {
        return Err(ArtifactError::EmptyWebpDescription);
    }
    if payload.starts_with(b"{") {
        return Ok(payload.to_vec());
    }
    STANDARD
        .decode(payload)
        .map_err(ArtifactError::InvalidBase64WebpMetadata)
}

pub(super) fn extract_charx(source: &[u8]) -> Result<CharxArchive, ArtifactError> {
    let mut archive =
        zip::ZipArchive::new(Cursor::new(source)).map_err(ArtifactError::InvalidCharx)?;
    let mut paths = HashSet::with_capacity(archive.len());
    let mut card_json = None;
    let mut lorebooks = Vec::new();
    let mut assets = Vec::new();
    let mut extracted_size = 0_u64;
    let limit = crate::limits::MAX_CHARX_UNCOMPRESSED_BYTES as u64;

    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(ArtifactError::InvalidCharx)?;
        let logical_path = safe_archive_path(&entry)?;
        if !paths.insert(logical_path.clone()) {
            return Err(ArtifactError::DuplicateArchivePath(logical_path));
        }
        if entry.is_symlink() || (!entry.is_file() && !entry.is_dir()) {
            return Err(ArtifactError::UnsupportedArchiveEntry(logical_path));
        }
        if entry.encrypted() {
            return Err(ArtifactError::EncryptedCharx);
        }
        if entry.is_dir() {
            continue;
        }

        let remaining = limit.saturating_sub(extracted_size);
        if entry.size() > remaining {
            return Err(ArtifactError::CharxTooLarge {
                size: extracted_size.saturating_add(entry.size()),
                limit,
            });
        }
        let mut bytes = Vec::with_capacity(entry.size() as usize);
        entry
            .by_ref()
            .take(remaining + 1)
            .read_to_end(&mut bytes)
            .map_err(ArtifactError::ReadCharx)?;
        if bytes.len() as u64 > remaining {
            return Err(ArtifactError::CharxTooLarge {
                size: extracted_size.saturating_add(bytes.len() as u64),
                limit,
            });
        }
        extracted_size += bytes.len() as u64;

        if logical_path == "card.json" {
            card_json = Some(bytes);
        } else if logical_path
            .rsplit('/')
            .next()
            .is_some_and(|name| name == "lorebook.json")
        {
            lorebooks.push(bytes);
        } else if is_media_path(&logical_path) {
            assets.push(CharxAsset {
                logical_path,
                bytes,
            });
        }
    }

    Ok(CharxArchive {
        card_json: card_json.ok_or(ArtifactError::MissingCharxCard)?,
        lorebooks,
        assets,
    })
}

fn safe_archive_path<R: Read>(entry: &zip::read::ZipFile<'_, R>) -> Result<String, ArtifactError> {
    let name = str::from_utf8(entry.name_raw())
        .map_err(|_| ArtifactError::UnsafeArchivePath(entry.name().to_owned()))?;
    let trimmed = name.strip_suffix('/').unwrap_or(name);
    if trimmed.is_empty()
        || trimmed.starts_with('/')
        || trimmed.contains('\\')
        || trimmed
            .split('/')
            .any(|component| component.is_empty() || matches!(component, "." | ".."))
        || entry.enclosed_name().is_none()
    {
        return Err(ArtifactError::UnsafeArchivePath(name.to_owned()));
    }
    Ok(trimmed.to_owned())
}

pub(super) fn is_media_path(path: &str) -> bool {
    path.rsplit_once('.')
        .map(|(_, extension)| extension)
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "png" | "apng" | "webp" | "jpg" | "jpeg" | "gif" | "avif" | "wav" | "mp3" | "ogg"
            )
        })
}

#[derive(Clone, Copy)]
enum MetadataCandidate<'a> {
    Text(&'a [u8]),
    InternationalText(&'a [u8]),
}

impl MetadataCandidate<'_> {
    fn decode(self) -> Result<Vec<u8>, ArtifactError> {
        match self {
            Self::Text(text) => decode_json_payload(text),
            Self::InternationalText(data) => {
                let text = decode_itxt_text(data)?;
                decode_json_payload(&text)
            }
        }
    }
}

pub(super) fn extract_png_card(source: &[u8]) -> Result<Vec<u8>, ArtifactError> {
    if !source.starts_with(PNG_SIGNATURE) {
        return Err(ArtifactError::InvalidPng("invalid PNG signature"));
    }

    let mut offset = PNG_SIGNATURE.len();
    let mut ccv3_itxt = None;
    let mut chara_itxt = None;
    let mut chara_text = None;
    let mut complete = false;

    while offset < source.len() {
        let header_end = offset.checked_add(8).ok_or(ArtifactError::TruncatedPng)?;
        if header_end > source.len() {
            return Err(ArtifactError::TruncatedPng);
        }
        let length = u32::from_be_bytes(
            source[offset..offset + 4]
                .try_into()
                .expect("PNG length contains four bytes"),
        ) as usize;
        let chunk_end = header_end
            .checked_add(length)
            .and_then(|end| end.checked_add(4))
            .ok_or(ArtifactError::TruncatedPng)?;
        if chunk_end > source.len() {
            return Err(ArtifactError::TruncatedPng);
        }

        let kind = &source[offset + 4..header_end];
        let data = &source[header_end..header_end + length];
        let expected_crc = u32::from_be_bytes(
            source[header_end + length..chunk_end]
                .try_into()
                .expect("PNG CRC contains four bytes"),
        );
        let mut hasher = Hasher::new();
        hasher.update(kind);
        hasher.update(data);
        if hasher.finalize() != expected_crc {
            return Err(ArtifactError::InvalidPng("chunk CRC mismatch"));
        }

        match kind {
            b"tEXt" => {
                if let Some((keyword, text)) = split_once_nul(data)
                    && keyword == b"chara"
                    && chara_text.is_none()
                {
                    chara_text = Some(MetadataCandidate::Text(text));
                }
            }
            b"iTXt" => {
                if let Some((keyword, rest)) = split_once_nul(data)
                    && matches!(keyword, b"chara" | b"ccv3")
                {
                    let candidate = MetadataCandidate::InternationalText(rest);
                    if keyword == b"ccv3" && ccv3_itxt.is_none() {
                        ccv3_itxt = Some(candidate);
                    } else if keyword == b"chara" && chara_itxt.is_none() {
                        chara_itxt = Some(candidate);
                    }
                }
            }
            b"IEND" if data.is_empty() => {
                complete = true;
                break;
            }
            b"IEND" => return Err(ArtifactError::InvalidPng("IEND chunk is not empty")),
            _ => {}
        }
        offset = chunk_end;
    }
    if !complete {
        return Err(ArtifactError::TruncatedPng);
    }

    ccv3_itxt
        .or(chara_itxt)
        .or(chara_text)
        .ok_or(ArtifactError::MissingPngMetadata)?
        .decode()
}

fn decode_itxt_text(data: &[u8]) -> Result<Vec<u8>, ArtifactError> {
    let (&compression_flag, rest) = data
        .split_first()
        .ok_or(ArtifactError::InvalidPng("truncated iTXt chunk"))?;
    let (&compression_method, rest) = rest
        .split_first()
        .ok_or(ArtifactError::InvalidPng("truncated iTXt chunk"))?;
    let (_, rest) =
        split_once_nul(rest).ok_or(ArtifactError::InvalidPng("truncated iTXt language tag"))?;
    let (_, text) = split_once_nul(rest).ok_or(ArtifactError::InvalidPng(
        "truncated iTXt translated keyword",
    ))?;

    match (compression_flag, compression_method) {
        (0, 0) => Ok(text.to_vec()),
        (1, 0) => {
            let mut decoded = Vec::new();
            ZlibDecoder::new(text)
                .take((crate::limits::MAX_ARTIFACT_BYTES + 1) as u64)
                .read_to_end(&mut decoded)
                .map_err(ArtifactError::InvalidCompressedPngMetadata)?;
            Ok(decoded)
        }
        (0 | 1, _) => Err(ArtifactError::InvalidPng(
            "unsupported iTXt compression method",
        )),
        _ => Err(ArtifactError::InvalidPng("invalid iTXt compression flag")),
    }
}

fn decode_json_payload(payload: &[u8]) -> Result<Vec<u8>, ArtifactError> {
    let trimmed = trim_ascii(payload);
    if trimmed.starts_with(b"{") {
        return Ok(trimmed.to_vec());
    }
    STANDARD
        .decode(trimmed)
        .map_err(ArtifactError::InvalidBase64PngMetadata)
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
