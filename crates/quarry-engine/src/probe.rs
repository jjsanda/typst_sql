//! Database header probing (the catchable pre-check behind `qr.try-sqlite`).
//!
//! Parses the 100-byte SQLite header without opening the database, so a
//! document can inspect validity, page geometry and WAL state, and the open
//! path can produce precise `source-unsupported` errors (C-7).

use crate::error::{codes, QuarryError};
use ciborium::Value;

pub const HEADER_MAGIC: &[u8; 16] = b"SQLite format 3\0";

#[derive(Debug, Clone, PartialEq)]
pub struct HeaderInfo {
    pub page_size: u32,
    pub page_count: u32,
    /// File format write version: 1 = legacy/rollback, 2 = WAL.
    pub write_version: u8,
    pub read_version: u8,
    pub schema_cookie: u32,
    pub sqlite_version_number: u32,
    pub application_id: u32,
    pub user_version: i32,
    pub text_encoding: u32,
}

/// Parse the header, or explain exactly why the bytes are not a usable database.
pub fn parse_header(bytes: &[u8]) -> Result<HeaderInfo, QuarryError> {
    if bytes.is_empty() {
        return Err(QuarryError::new(
            codes::SOURCE_INVALID,
            "database is empty (0 bytes) — is the file path correct?",
        ));
    }
    if bytes.len() < 100 {
        return Err(QuarryError::new(
            codes::SOURCE_INVALID,
            format!(
                "database is {} bytes, smaller than the 100-byte SQLite header — the file is truncated or not a database",
                bytes.len()
            ),
        ));
    }
    if &bytes[0..16] != HEADER_MAGIC {
        return Err(QuarryError::new(
            codes::SOURCE_INVALID,
            "not a SQLite database (header magic mismatch); quarry reads SQLite files — for CSV or Parquet use the sidecar's analytics source",
        ));
    }
    let be16 = |o: usize| u16::from_be_bytes([bytes[o], bytes[o + 1]]) as u32;
    let be32 = |o: usize| u32::from_be_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);
    let raw_page_size = be16(16);
    let page_size = if raw_page_size == 1 {
        65_536
    } else {
        raw_page_size
    };
    if !(512..=65_536).contains(&page_size) || !page_size.is_power_of_two() {
        return Err(QuarryError::new(
            codes::SOURCE_INVALID,
            format!("invalid page size {page_size} in database header — the file is corrupt"),
        ));
    }
    let info = HeaderInfo {
        page_size,
        page_count: be32(28),
        write_version: bytes[18],
        read_version: bytes[19],
        schema_cookie: be32(40),
        text_encoding: be32(56),
        user_version: be32(60) as i32,
        application_id: be32(68),
        sqlite_version_number: be32(96),
    };
    if info.write_version > 2 || info.read_version > 2 {
        return Err(QuarryError::new(
            codes::SOURCE_UNSUPPORTED,
            format!(
                "unknown file format version {}/{} — produced by a newer SQLite?",
                info.write_version, info.read_version
            ),
        ));
    }
    Ok(info)
}

/// The remedy we print for WAL-mode databases that cannot be normalized.
pub fn wal_remedy(name: &str) -> String {
    format!(
        "run `sqlite3 {name} \"PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode=DELETE;\"` \
         to checkpoint and convert it, then rebuild"
    )
}

/// CBOR summary for the `probe` plugin export.
pub fn probe_value(bytes: &[u8]) -> Value {
    match parse_header(bytes) {
        Ok(h) => Value::Map(vec![
            (Value::Text("ok".into()), Value::Bool(true)),
            (
                Value::Text("page-size".into()),
                Value::from(h.page_size as u64),
            ),
            (
                Value::Text("page-count".into()),
                Value::from(h.page_count as u64),
            ),
            (
                Value::Text("size-bytes".into()),
                Value::from(h.page_size as u64 * h.page_count as u64),
            ),
            (Value::Text("wal".into()), Value::Bool(h.write_version == 2)),
            (
                Value::Text("schema-cookie".into()),
                Value::from(h.schema_cookie as u64),
            ),
            (
                Value::Text("user-version".into()),
                Value::from(h.user_version as i64),
            ),
            (
                Value::Text("application-id".into()),
                Value::from(h.application_id as u64),
            ),
            (
                Value::Text("text-encoding".into()),
                Value::Text(
                    match h.text_encoding {
                        1 => "utf-8",
                        2 => "utf-16le",
                        3 => "utf-16be",
                        _ => "unknown",
                    }
                    .into(),
                ),
            ),
        ]),
        Err(e) => Value::Map(vec![
            (Value::Text("ok".into()), Value::Bool(false)),
            (Value::Text("code".into()), Value::Text(e.code)),
            (Value::Text("message".into()), Value::Text(e.message)),
        ]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_header() -> Vec<u8> {
        let mut h = vec![0u8; 100];
        h[0..16].copy_from_slice(HEADER_MAGIC);
        h[16..18].copy_from_slice(&4096u16.to_be_bytes());
        h[18] = 1;
        h[19] = 1;
        h[28..32].copy_from_slice(&1u32.to_be_bytes());
        h[56..60].copy_from_slice(&1u32.to_be_bytes());
        h
    }

    #[test]
    fn accepts_valid_header() {
        let h = parse_header(&minimal_header()).unwrap();
        assert_eq!(h.page_size, 4096);
        assert_eq!(h.write_version, 1);
    }

    #[test]
    fn rejects_empty_truncated_and_garbage() {
        assert_eq!(parse_header(&[]).unwrap_err().code, codes::SOURCE_INVALID);
        assert_eq!(
            parse_header(&[0u8; 50]).unwrap_err().code,
            codes::SOURCE_INVALID
        );
        let mut garbage = minimal_header();
        garbage[0] = b'X';
        assert_eq!(
            parse_header(&garbage).unwrap_err().code,
            codes::SOURCE_INVALID
        );
    }

    #[test]
    fn rejects_bad_page_size() {
        let mut h = minimal_header();
        h[16..18].copy_from_slice(&1000u16.to_be_bytes()); // not a power of two
        assert_eq!(parse_header(&h).unwrap_err().code, codes::SOURCE_INVALID);
    }

    #[test]
    fn page_size_1_means_64k() {
        let mut h = minimal_header();
        h[16..18].copy_from_slice(&1u16.to_be_bytes());
        assert_eq!(parse_header(&h).unwrap().page_size, 65_536);
    }

    #[test]
    fn detects_wal() {
        let mut h = minimal_header();
        h[18] = 2;
        h[19] = 2;
        assert_eq!(parse_header(&h).unwrap().write_version, 2);
    }
}
