//! Streaming GGUF representation inspection for managed Local AI registration.
//!
//! Registration only needs GGUF metadata and tensor descriptors. The tensor data
//! blob can be many gigabytes, so this parser never maps or reads that payload.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

const GGUF_MAGIC: &[u8; 4] = b"GGUF";
const GGUF_MIN_SUPPORTED_VERSION: u32 = 2;
const GGUF_MAX_SUPPORTED_VERSION: u32 = 3;
const GGUF_TYPE_UINT8: u32 = 0;
const GGUF_TYPE_INT8: u32 = 1;
const GGUF_TYPE_UINT16: u32 = 2;
const GGUF_TYPE_INT16: u32 = 3;
const GGUF_TYPE_UINT32: u32 = 4;
const GGUF_TYPE_INT32: u32 = 5;
const GGUF_TYPE_FLOAT32: u32 = 6;
const GGUF_TYPE_BOOL: u32 = 7;
const GGUF_TYPE_STRING: u32 = 8;
const GGUF_TYPE_ARRAY: u32 = 9;
const GGUF_TYPE_UINT64: u32 = 10;
const GGUF_TYPE_INT64: u32 = 11;
const GGUF_TYPE_FLOAT64: u32 = 12;
const MAX_GGUF_METADATA_ENTRIES: u64 = 1_000_000;
const MAX_GGUF_TENSORS: u64 = 1_000_000;
const MAX_GGUF_STRING_BYTES: u64 = 16 * 1024 * 1024;
const MAX_GGUF_ARRAY_ELEMENTS: u64 = 16_000_000;
const REPRESENTATION_PREFIX: &str = "gguf-repr-v1;";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct GgufRepresentation {
    pub(super) descriptor: String,
    pub(super) canonical_quantization: Option<String>,
}

pub(super) fn inspect_representation(path: &Path) -> io::Result<GgufRepresentation> {
    let mut file = File::open(path)?;
    let file_len = file.metadata()?.len();
    inspect_reader(&mut file, file_len)
}

pub(super) fn is_representation_descriptor(value: &str) -> bool {
    value.starts_with(REPRESENTATION_PREFIX)
        && value.contains(";gguf=")
        && value.contains(";types=")
        && !value.ends_with(";types=")
}

fn inspect_reader<R: Read + Seek>(
    reader: &mut R,
    file_len: u64,
) -> io::Result<GgufRepresentation> {
    let mut magic = [0_u8; 4];
    reader.read_exact(&mut magic)?;
    if &magic != GGUF_MAGIC {
        return Err(invalid_data("file does not start with GGUF magic"));
    }

    let version = read_u32(reader)?;
    if !(GGUF_MIN_SUPPORTED_VERSION..=GGUF_MAX_SUPPORTED_VERSION).contains(&version) {
        return Err(invalid_data(format!(
            "unsupported GGUF version {version}; supported versions are {GGUF_MIN_SUPPORTED_VERSION}..={GGUF_MAX_SUPPORTED_VERSION}"
        )));
    }

    let tensor_count = read_u64(reader)?;
    let metadata_count = read_u64(reader)?;
    if tensor_count == 0 || tensor_count > MAX_GGUF_TENSORS {
        return Err(invalid_data(format!(
            "GGUF tensor count {tensor_count} is outside the supported registration range"
        )));
    }
    if metadata_count > MAX_GGUF_METADATA_ENTRIES {
        return Err(invalid_data(format!(
            "GGUF metadata count {metadata_count} is outside the supported registration range"
        )));
    }

    let mut file_type = None;
    let mut quantization_version = None;
    for _ in 0..metadata_count {
        let key = read_string(reader)?;
        let value_type = read_u32(reader)?;
        match key.as_str() {
            "general.file_type" => {
                if value_type != GGUF_TYPE_UINT32 {
                    return Err(invalid_data(
                        "GGUF general.file_type is not encoded as UINT32",
                    ));
                }
                if file_type.replace(read_u32(reader)?).is_some() {
                    return Err(invalid_data("GGUF general.file_type is duplicated"));
                }
            }
            "general.quantization_version" => {
                if value_type != GGUF_TYPE_UINT32 {
                    return Err(invalid_data(
                        "GGUF general.quantization_version is not encoded as UINT32",
                    ));
                }
                if quantization_version.replace(read_u32(reader)?).is_some() {
                    return Err(invalid_data(
                        "GGUF general.quantization_version is duplicated",
                    ));
                }
            }
            _ => skip_value(reader, file_len, value_type)?,
        }
    }

    let mut tensor_types = BTreeMap::<u32, u64>::new();
    for _ in 0..tensor_count {
        skip_string(reader, file_len)?;
        let dimensions = read_u32(reader)?;
        if !(1..=4).contains(&dimensions) {
            return Err(invalid_data(format!(
                "GGUF tensor has unsupported dimension count {dimensions}"
            )));
        }
        for _ in 0..dimensions {
            let dimension = read_u64(reader)?;
            if dimension == 0 || dimension > i64::MAX as u64 {
                return Err(invalid_data(format!(
                    "GGUF tensor has invalid dimension size {dimension}"
                )));
            }
        }
        let tensor_type = read_u32(reader)?;
        if ggml_type_name(tensor_type).is_none() {
            return Err(invalid_data(format!(
                "GGUF tensor uses unsupported ggml type {tensor_type}"
            )));
        }
        let count = tensor_types.entry(tensor_type).or_default();
        *count = count
            .checked_add(1)
            .ok_or_else(|| invalid_data("GGUF tensor type count overflowed"))?;
        let _offset = read_u64(reader)?;
    }

    let type_distribution = tensor_types
        .iter()
        .map(|(tensor_type, count)| {
            format!(
                "{}:{count}",
                ggml_type_name(*tensor_type).expect("validated GGML type must have a name")
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let descriptor = format!(
        "{REPRESENTATION_PREFIX}gguf={version};file_type={};quantization_version={};types={type_distribution}",
        optional_u32(file_type),
        optional_u32(quantization_version),
    );
    Ok(GgufRepresentation {
        descriptor,
        canonical_quantization: file_type
            .and_then(canonical_quantization_for_file_type)
            .map(str::to_owned),
    })
}

fn read_u32<R: Read>(reader: &mut R) -> io::Result<u32> {
    let mut bytes = [0_u8; 4];
    reader.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_u64<R: Read>(reader: &mut R) -> io::Result<u64> {
    let mut bytes = [0_u8; 8];
    reader.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

fn read_string<R: Read>(reader: &mut R) -> io::Result<String> {
    let len = read_u64(reader)?;
    if len > MAX_GGUF_STRING_BYTES {
        return Err(invalid_data(format!(
            "GGUF string length {len} exceeds the registration parser limit"
        )));
    }
    let len = usize::try_from(len)
        .map_err(|_| invalid_data("GGUF string length does not fit this platform"))?;
    let mut bytes = vec![0_u8; len];
    reader.read_exact(&mut bytes)?;
    String::from_utf8(bytes).map_err(|error| invalid_data(format!("GGUF string is not UTF-8: {error}")))
}

fn skip_string<R: Read + Seek>(reader: &mut R, file_len: u64) -> io::Result<()> {
    let len = read_u64(reader)?;
    if len > MAX_GGUF_STRING_BYTES {
        return Err(invalid_data(format!(
            "GGUF string length {len} exceeds the registration parser limit"
        )));
    }
    skip_bytes(reader, file_len, len)
}

fn skip_value<R: Read + Seek>(
    reader: &mut R,
    file_len: u64,
    value_type: u32,
) -> io::Result<()> {
    match value_type {
        GGUF_TYPE_UINT8 | GGUF_TYPE_INT8 | GGUF_TYPE_BOOL => skip_bytes(reader, file_len, 1),
        GGUF_TYPE_UINT16 | GGUF_TYPE_INT16 => skip_bytes(reader, file_len, 2),
        GGUF_TYPE_UINT32 | GGUF_TYPE_INT32 | GGUF_TYPE_FLOAT32 => {
            skip_bytes(reader, file_len, 4)
        }
        GGUF_TYPE_UINT64 | GGUF_TYPE_INT64 | GGUF_TYPE_FLOAT64 => {
            skip_bytes(reader, file_len, 8)
        }
        GGUF_TYPE_STRING => skip_string(reader, file_len),
        GGUF_TYPE_ARRAY => {
            let element_type = read_u32(reader)?;
            if element_type == GGUF_TYPE_ARRAY {
                return Err(invalid_data("nested GGUF metadata arrays are unsupported"));
            }
            let count = read_u64(reader)?;
            if count > MAX_GGUF_ARRAY_ELEMENTS {
                return Err(invalid_data(format!(
                    "GGUF metadata array length {count} exceeds the registration parser limit"
                )));
            }
            if element_type == GGUF_TYPE_STRING {
                for _ in 0..count {
                    skip_string(reader, file_len)?;
                }
                return Ok(());
            }
            let element_size = fixed_gguf_type_size(element_type).ok_or_else(|| {
                invalid_data(format!(
                    "GGUF metadata array uses unsupported element type {element_type}"
                ))
            })?;
            let bytes = count
                .checked_mul(element_size)
                .ok_or_else(|| invalid_data("GGUF metadata array byte size overflowed"))?;
            skip_bytes(reader, file_len, bytes)
        }
        _ => Err(invalid_data(format!(
            "GGUF metadata uses unsupported value type {value_type}"
        ))),
    }
}

fn fixed_gguf_type_size(value_type: u32) -> Option<u64> {
    match value_type {
        GGUF_TYPE_UINT8 | GGUF_TYPE_INT8 | GGUF_TYPE_BOOL => Some(1),
        GGUF_TYPE_UINT16 | GGUF_TYPE_INT16 => Some(2),
        GGUF_TYPE_UINT32 | GGUF_TYPE_INT32 | GGUF_TYPE_FLOAT32 => Some(4),
        GGUF_TYPE_UINT64 | GGUF_TYPE_INT64 | GGUF_TYPE_FLOAT64 => Some(8),
        GGUF_TYPE_STRING | GGUF_TYPE_ARRAY => None,
        _ => None,
    }
}

fn skip_bytes<R: Seek>(reader: &mut R, file_len: u64, bytes: u64) -> io::Result<()> {
    let position = reader.stream_position()?;
    let end = position
        .checked_add(bytes)
        .ok_or_else(|| invalid_data("GGUF offset overflowed"))?;
    if end > file_len {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "GGUF metadata or tensor descriptor is truncated",
        ));
    }
    reader.seek(SeekFrom::Start(end))?;
    Ok(())
}

fn optional_u32(value: Option<u32>) -> String {
    value.map_or_else(|| "none".to_owned(), |value| value.to_string())
}

fn canonical_quantization_for_file_type(file_type: u32) -> Option<&'static str> {
    match file_type {
        2 => Some("Q4_0"),
        3 => Some("Q4_1"),
        7 => Some("Q8_0"),
        8 => Some("Q5_0"),
        9 => Some("Q5_1"),
        10 => Some("Q2_K"),
        11 => Some("Q3_K_S"),
        12 => Some("Q3_K_M"),
        13 => Some("Q3_K_L"),
        14 => Some("Q4_K_S"),
        15 => Some("Q4_K_M"),
        16 => Some("Q5_K_S"),
        17 => Some("Q5_K_M"),
        18 => Some("Q6_K"),
        19 => Some("IQ2_XXS"),
        20 => Some("IQ2_XS"),
        21 => Some("Q2_K_S"),
        22 => Some("IQ3_XS"),
        23 => Some("IQ3_XXS"),
        24 => Some("IQ1_S"),
        25 => Some("IQ4_NL"),
        26 => Some("IQ3_S"),
        27 => Some("IQ3_M"),
        28 => Some("IQ2_S"),
        29 => Some("IQ2_M"),
        30 => Some("IQ4_XS"),
        31 => Some("IQ1_M"),
        36 => Some("TQ1_0"),
        37 => Some("TQ2_0"),
        38 => Some("MXFP4_MOE"),
        39 => Some("NVFP4"),
        40 => Some("Q1_0"),
        41 => Some("Q2_0"),
        _ => None,
    }
}

fn ggml_type_name(tensor_type: u32) -> Option<&'static str> {
    match tensor_type {
        0 => Some("F32"),
        1 => Some("F16"),
        2 => Some("Q4_0"),
        3 => Some("Q4_1"),
        6 => Some("Q5_0"),
        7 => Some("Q5_1"),
        8 => Some("Q8_0"),
        9 => Some("Q8_1"),
        10 => Some("Q2_K"),
        11 => Some("Q3_K"),
        12 => Some("Q4_K"),
        13 => Some("Q5_K"),
        14 => Some("Q6_K"),
        15 => Some("Q8_K"),
        16 => Some("IQ2_XXS"),
        17 => Some("IQ2_XS"),
        18 => Some("IQ3_XXS"),
        19 => Some("IQ1_S"),
        20 => Some("IQ4_NL"),
        21 => Some("IQ3_S"),
        22 => Some("IQ2_S"),
        23 => Some("IQ4_XS"),
        24 => Some("I8"),
        25 => Some("I16"),
        26 => Some("I32"),
        27 => Some("I64"),
        28 => Some("F64"),
        29 => Some("IQ1_M"),
        30 => Some("BF16"),
        34 => Some("TQ1_0"),
        35 => Some("TQ2_0"),
        39 => Some("MXFP4"),
        40 => Some("NVFP4"),
        41 => Some("Q1_0"),
        42 => Some("Q2_0"),
        _ => None,
    }
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[cfg(test)]
pub(super) fn write_test_gguf(
    path: &Path,
    file_type: Option<u32>,
    tensor_types: &[u32],
) -> io::Result<()> {
    std::fs::write(path, build_test_gguf(3, file_type, Some(2), tensor_types))
}

#[cfg(test)]
fn build_test_gguf(
    version: u32,
    file_type: Option<u32>,
    quantization_version: Option<u32>,
    tensor_types: &[u32],
) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(GGUF_MAGIC);
    bytes.extend_from_slice(&version.to_le_bytes());
    bytes.extend_from_slice(&(tensor_types.len() as u64).to_le_bytes());
    let metadata_count = file_type.is_some() as u64 + quantization_version.is_some() as u64;
    bytes.extend_from_slice(&metadata_count.to_le_bytes());

    if let Some(file_type) = file_type {
        push_string(&mut bytes, "general.file_type");
        bytes.extend_from_slice(&GGUF_TYPE_UINT32.to_le_bytes());
        bytes.extend_from_slice(&file_type.to_le_bytes());
    }
    if let Some(quantization_version) = quantization_version {
        push_string(&mut bytes, "general.quantization_version");
        bytes.extend_from_slice(&GGUF_TYPE_UINT32.to_le_bytes());
        bytes.extend_from_slice(&quantization_version.to_le_bytes());
    }

    for (index, tensor_type) in tensor_types.iter().copied().enumerate() {
        push_string(&mut bytes, &format!("tensor.{index}"));
        bytes.extend_from_slice(&1_u32.to_le_bytes());
        bytes.extend_from_slice(&32_u64.to_le_bytes());
        bytes.extend_from_slice(&tensor_type.to_le_bytes());
        bytes.extend_from_slice(&0_u64.to_le_bytes());
    }
    bytes
}

#[cfg(test)]
fn push_string(bytes: &mut Vec<u8>, value: &str) {
    bytes.extend_from_slice(&(value.len() as u64).to_le_bytes());
    bytes.extend_from_slice(value.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn mixed_tensor_types_produce_a_stable_distribution_without_single_quantization() {
        let bytes = build_test_gguf(3, None, Some(2), &[12, 14, 12, 0]);
        let mut reader = Cursor::new(bytes.clone());
        let representation = inspect_reader(&mut reader, bytes.len() as u64).expect("representation");
        assert_eq!(representation.canonical_quantization, None);
        assert_eq!(
            representation.descriptor,
            "gguf-repr-v1;gguf=3;file_type=none;quantization_version=2;types=F32:1,Q4_K:2,Q6_K:1"
        );
        assert!(is_representation_descriptor(&representation.descriptor));
    }

    #[test]
    fn general_file_type_supplies_only_a_canonical_label_while_descriptor_stays_exact() {
        let bytes = build_test_gguf(3, Some(15), Some(2), &[12, 12, 14]);
        let mut reader = Cursor::new(bytes.clone());
        let representation = inspect_reader(&mut reader, bytes.len() as u64).expect("representation");
        assert_eq!(
            representation.canonical_quantization.as_deref(),
            Some("Q4_K_M")
        );
        assert_eq!(
            representation.descriptor,
            "gguf-repr-v1;gguf=3;file_type=15;quantization_version=2;types=Q4_K:2,Q6_K:1"
        );
    }

    #[test]
    fn malformed_or_truncated_gguf_is_rejected() {
        let mut bytes = build_test_gguf(3, Some(15), Some(2), &[12]);
        bytes.truncate(bytes.len() - 3);
        let mut reader = Cursor::new(bytes.clone());
        assert!(inspect_reader(&mut reader, bytes.len() as u64).is_err());

        let mut reader = Cursor::new(b"not-gguf".to_vec());
        assert!(inspect_reader(&mut reader, 8).is_err());
    }

    #[test]
    fn unsupported_gguf_version_is_rejected() {
        let bytes = build_test_gguf(4, Some(15), Some(2), &[12]);
        let mut reader = Cursor::new(bytes.clone());
        let error = inspect_reader(&mut reader, bytes.len() as u64).expect_err("version");
        assert!(error.to_string().contains("unsupported GGUF version 4"));
    }

    #[test]
    fn unsupported_tensor_type_is_rejected() {
        let bytes = build_test_gguf(3, None, Some(2), &[999]);
        let mut reader = Cursor::new(bytes.clone());
        let error = inspect_reader(&mut reader, bytes.len() as u64).expect_err("type");
        assert!(error.to_string().contains("unsupported ggml type 999"));
    }
}
