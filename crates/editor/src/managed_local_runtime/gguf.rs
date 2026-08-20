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

/// Architecture-independent metadata keys the runtime derives launch policy from.
///
/// Every key is namespaced by the value of `general.architecture`, so the parser
/// resolves them after the metadata block is read instead of assuming any model
/// family. A model that omits a key simply leaves the derived value unmeasured.
const ARCHITECTURE_SCALAR_SUFFIXES: [&str; 8] = [
    ".context_length",
    ".block_count",
    ".embedding_length",
    ".attention.head_count",
    ".attention.head_count_kv",
    ".attention.key_length",
    ".attention.value_length",
    ".attention.sliding_window",
];
const ARCHITECTURE_KEY: &str = "general.architecture";
const CHAT_TEMPLATE_KEY: &str = "tokenizer.chat_template";
/// Bytes one cached key or value element occupies at the default f16 KV cache type.
const KV_CACHE_ELEMENT_BYTES: u64 = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct GgufRepresentation {
    pub(super) descriptor: String,
    pub(super) canonical_quantization: Option<String>,
    pub(super) capability: GgufModelCapability,
}

/// Launch-relevant model shape measured from GGUF metadata.
///
/// The values are reported exactly as the file declares them. Deriving a
/// context window or a KV budget from them belongs to the runtime policy, not
/// to this parser, and nothing here is specific to one model family.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct GgufModelCapability {
    /// Value of `general.architecture`, used only to namespace the other keys.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) architecture: Option<String>,
    /// Context window the model was trained for, when declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) train_context_tokens: Option<u32>,
    /// KV cache bytes one token occupies across every block, when derivable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) kv_cache_bytes_per_token: Option<u64>,
    /// Declared attention sliding window, when the architecture uses one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) sliding_window_tokens: Option<u32>,
    /// Whether the file carries its own chat template.
    #[serde(default)]
    pub(crate) chat_template: bool,
}

impl GgufModelCapability {
    fn derive(
        architecture: Option<String>,
        scalars: &BTreeMap<String, u64>,
        chat_template: bool,
    ) -> Self {
        let namespace = architecture.clone();
        let lookup = |suffix: &str| -> Option<u64> {
            let namespace = namespace.as_deref()?;
            scalars.get(&format!("{namespace}{suffix}")).copied()
        };
        let block_count = lookup(".block_count");
        let embedding_length = lookup(".embedding_length");
        let head_count = lookup(".attention.head_count");
        let head_count_kv = lookup(".attention.head_count_kv").or(head_count);
        let key_length =
            lookup(".attention.key_length").or_else(|| match (embedding_length, head_count) {
                (Some(embedding), Some(heads)) if heads > 0 => Some(embedding / heads),
                _ => None,
            });
        let value_length = lookup(".attention.value_length").or(key_length);
        let kv_cache_bytes_per_token = match (block_count, head_count_kv, key_length, value_length)
        {
            (Some(blocks), Some(heads), Some(key), Some(value)) => key
                .checked_add(value)
                .and_then(|element| element.checked_mul(heads))
                .and_then(|per_block| per_block.checked_mul(blocks))
                .and_then(|total| total.checked_mul(KV_CACHE_ELEMENT_BYTES))
                .filter(|total| *total > 0),
            _ => None,
        };
        Self {
            architecture,
            train_context_tokens: lookup(".context_length")
                .and_then(|value| u32::try_from(value).ok()),
            kv_cache_bytes_per_token,
            sliding_window_tokens: lookup(".attention.sliding_window")
                .and_then(|value| u32::try_from(value).ok())
                .filter(|value| *value > 0),
            chat_template,
        }
    }
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

fn inspect_reader<R: Read + Seek>(reader: &mut R, file_len: u64) -> io::Result<GgufRepresentation> {
    let mut magic = [0_u8; 4];
    reader.read_exact(&mut magic)?;
    if magic != *GGUF_MAGIC {
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
    if !(1..=MAX_GGUF_TENSORS).contains(&tensor_count) {
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
    let mut architecture = None;
    let mut chat_template = false;
    let mut scalars = BTreeMap::<String, u64>::new();
    for _ in 0..metadata_count {
        let key = read_string(reader)?;
        let value_type = read_u32(reader)?;
        if key == ARCHITECTURE_KEY && value_type == GGUF_TYPE_STRING {
            architecture = Some(read_string(reader)?);
            continue;
        }
        if key == CHAT_TEMPLATE_KEY {
            chat_template = true;
            skip_value(reader, file_len, value_type)?;
            continue;
        }
        if ARCHITECTURE_SCALAR_SUFFIXES
            .iter()
            .any(|suffix| key.ends_with(suffix))
        {
            if let Some(value) = read_scalar_u64(reader, file_len, value_type)? {
                scalars.insert(key, value);
            }
            continue;
        }
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
            if !(1..=i64::MAX as u64).contains(&dimension) {
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
        capability: GgufModelCapability::derive(architecture, &scalars, chat_template),
    })
}

/// Reads one numeric metadata value, or skips a value the runtime cannot use.
///
/// Returning `Ok(None)` keeps the caller advancing through the metadata block
/// when a key it recognizes by name carries a type it cannot interpret.
fn read_scalar_u64<R: Read + Seek>(
    reader: &mut R,
    file_len: u64,
    value_type: u32,
) -> io::Result<Option<u64>> {
    match value_type {
        GGUF_TYPE_UINT32 => Ok(Some(u64::from(read_u32(reader)?))),
        GGUF_TYPE_INT32 => {
            let value = read_u32(reader)? as i32;
            Ok(u64::try_from(value).ok())
        }
        GGUF_TYPE_UINT64 => Ok(Some(read_u64(reader)?)),
        GGUF_TYPE_INT64 => {
            let value = read_u64(reader)? as i64;
            Ok(u64::try_from(value).ok())
        }
        GGUF_TYPE_UINT16 => {
            let mut bytes = [0_u8; 2];
            reader.read_exact(&mut bytes)?;
            Ok(Some(u64::from(u16::from_le_bytes(bytes))))
        }
        _ => {
            skip_value(reader, file_len, value_type)?;
            Ok(None)
        }
    }
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
    String::from_utf8(bytes)
        .map_err(|error| invalid_data(format!("GGUF string is not UTF-8: {error}")))
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

fn skip_value<R: Read + Seek>(reader: &mut R, file_len: u64, value_type: u32) -> io::Result<()> {
    match value_type {
        GGUF_TYPE_UINT8 | GGUF_TYPE_INT8 | GGUF_TYPE_BOOL => skip_bytes(reader, file_len, 1),
        GGUF_TYPE_UINT16 | GGUF_TYPE_INT16 => skip_bytes(reader, file_len, 2),
        GGUF_TYPE_UINT32 | GGUF_TYPE_INT32 | GGUF_TYPE_FLOAT32 => skip_bytes(reader, file_len, 4),
        GGUF_TYPE_UINT64 | GGUF_TYPE_INT64 | GGUF_TYPE_FLOAT64 => skip_bytes(reader, file_len, 8),
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

#[cfg(feature = "visual-validation")]
pub(super) fn write_visual_validation_gguf(path: &Path) -> io::Result<()> {
    std::fs::write(
        path,
        build_test_gguf(3, Some(15), Some(2), &[12, 12, 14], None, &[]),
    )
}

#[cfg(test)]
pub(super) fn write_test_gguf(
    path: &Path,
    file_type: Option<u32>,
    tensor_types: &[u32],
) -> io::Result<()> {
    std::fs::write(
        path,
        build_test_gguf(3, file_type, Some(2), tensor_types, None, &[]),
    )
}

#[cfg(test)]
pub(super) fn write_test_gguf_with_architecture(
    path: &Path,
    architecture: &str,
    scalars: &[(&str, u64)],
) -> io::Result<()> {
    std::fs::write(
        path,
        build_test_gguf(
            3,
            Some(15),
            Some(2),
            &[12, 12, 14],
            Some(architecture),
            scalars,
        ),
    )
}

#[cfg(any(test, feature = "visual-validation"))]
fn build_test_gguf(
    version: u32,
    file_type: Option<u32>,
    quantization_version: Option<u32>,
    tensor_types: &[u32],
    architecture: Option<&str>,
    scalars: &[(&str, u64)],
) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(GGUF_MAGIC);
    bytes.extend_from_slice(&version.to_le_bytes());
    bytes.extend_from_slice(&(tensor_types.len() as u64).to_le_bytes());
    let metadata_count = file_type.is_some() as u64
        + quantization_version.is_some() as u64
        + architecture.map_or(0, |_| 1 + scalars.len() as u64);
    bytes.extend_from_slice(&metadata_count.to_le_bytes());

    if let Some(architecture) = architecture {
        push_string(&mut bytes, ARCHITECTURE_KEY);
        bytes.extend_from_slice(&GGUF_TYPE_STRING.to_le_bytes());
        push_string(&mut bytes, architecture);
        for (suffix, value) in scalars {
            push_string(&mut bytes, &format!("{architecture}{suffix}"));
            bytes.extend_from_slice(&GGUF_TYPE_UINT32.to_le_bytes());
            bytes.extend_from_slice(&(*value as u32).to_le_bytes());
        }
    }
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

#[cfg(any(test, feature = "visual-validation"))]
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
        let bytes = build_test_gguf(3, None, Some(2), &[12, 14, 12, 0], None, &[]);
        let mut reader = Cursor::new(bytes.as_slice());
        let representation =
            inspect_reader(&mut reader, bytes.len() as u64).expect("representation");
        assert_eq!(representation.canonical_quantization, None);
        assert_eq!(
            representation.descriptor,
            "gguf-repr-v1;gguf=3;file_type=none;quantization_version=2;types=F32:1,Q4_K:2,Q6_K:1"
        );
        assert!(is_representation_descriptor(&representation.descriptor));
    }

    #[test]
    fn general_file_type_supplies_only_a_canonical_label_while_descriptor_stays_exact() {
        let bytes = build_test_gguf(3, Some(15), Some(2), &[12, 12, 14], None, &[]);
        let mut reader = Cursor::new(bytes.as_slice());
        let representation =
            inspect_reader(&mut reader, bytes.len() as u64).expect("representation");
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
        let mut bytes = build_test_gguf(3, Some(15), Some(2), &[12], None, &[]);
        bytes.truncate(bytes.len() - 3);
        let mut reader = Cursor::new(bytes.as_slice());
        assert!(inspect_reader(&mut reader, bytes.len() as u64).is_err());

        let mut reader = Cursor::new(b"not-gguf");
        assert!(inspect_reader(&mut reader, 8).is_err());
    }

    #[test]
    fn unsupported_gguf_version_is_rejected() {
        let bytes = build_test_gguf(4, Some(15), Some(2), &[12], None, &[]);
        let mut reader = Cursor::new(bytes.as_slice());
        let error = inspect_reader(&mut reader, bytes.len() as u64).expect_err("version");
        assert!(error.to_string().contains("unsupported GGUF version 4"));
    }

    #[test]
    fn unsupported_tensor_type_is_rejected() {
        let bytes = build_test_gguf(3, None, Some(2), &[999], None, &[]);
        let mut reader = Cursor::new(bytes.as_slice());
        let error = inspect_reader(&mut reader, bytes.len() as u64).expect_err("type");
        assert!(error.to_string().contains("unsupported ggml type 999"));
    }

    #[test]
    fn architecture_namespaced_metadata_derives_a_generic_kv_cost() {
        let bytes = build_test_gguf(
            3,
            Some(15),
            Some(2),
            &[12, 12, 14],
            Some("any-architecture"),
            &[
                (".context_length", 131_072),
                (".block_count", 4),
                (".embedding_length", 512),
                (".attention.head_count", 8),
                (".attention.head_count_kv", 2),
            ],
        );
        let mut reader = Cursor::new(bytes.as_slice());
        let representation =
            inspect_reader(&mut reader, bytes.len() as u64).expect("representation");
        let capability = representation.capability;
        assert_eq!(capability.architecture.as_deref(), Some("any-architecture"));
        assert_eq!(capability.train_context_tokens, Some(131_072));
        // head_dim 64, two KV heads, key plus value, four blocks, two bytes each.
        assert_eq!(capability.kv_cache_bytes_per_token, Some(2_048));
        assert_eq!(capability.sliding_window_tokens, None);
    }

    #[test]
    fn a_model_without_architecture_metadata_reports_nothing_measured() {
        let bytes = build_test_gguf(3, Some(15), Some(2), &[12, 12, 14], None, &[]);
        let mut reader = Cursor::new(bytes.as_slice());
        let representation =
            inspect_reader(&mut reader, bytes.len() as u64).expect("representation");
        assert_eq!(representation.capability, GgufModelCapability::default());
    }

    #[test]
    fn sliding_window_metadata_is_reported_without_special_casing_any_family() {
        let bytes = build_test_gguf(
            3,
            Some(15),
            Some(2),
            &[12, 12, 14],
            Some("windowed"),
            &[
                (".context_length", 8_192),
                (".block_count", 2),
                (".attention.key_length", 128),
                (".attention.value_length", 128),
                (".attention.head_count_kv", 1),
                (".attention.sliding_window", 1_024),
            ],
        );
        let mut reader = Cursor::new(bytes.as_slice());
        let capability = inspect_reader(&mut reader, bytes.len() as u64)
            .expect("representation")
            .capability;
        assert_eq!(capability.sliding_window_tokens, Some(1_024));
        assert_eq!(capability.kv_cache_bytes_per_token, Some(1_024));
        assert_eq!(capability.train_context_tokens, Some(8_192));
    }
}
