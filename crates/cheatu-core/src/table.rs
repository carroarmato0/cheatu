//! Cheat table files: a small TOML format listing address expressions with a
//! type, a value and a frozen flag. Reading and writing only, no UI logic.
//!
//! Every problem fails the whole load with the offending line, so a broken
//! table is never half-loaded. Saves go to a temporary file that is renamed
//! over the target, so a failed save never corrupts an existing table.

use crate::addr_expr::AddrExpr;
use crate::value::ScanType;
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::fmt;
use std::io::{self, Write};
use std::path::Path;
use toml::Spanned;

/// Files larger than this are refused without being parsed.
pub const MAX_FILE_BYTES: u64 = 1 << 20;
/// Tables with more entries than this are refused.
pub const MAX_ENTRIES: usize = 10_000;

const FORMAT_VERSION: i64 = 1;

/// A loaded cheat table.
#[derive(Debug, Clone, PartialEq)]
pub struct CheatTable {
    /// Name of the process the table was made for (informational).
    pub process: Option<String>,
    pub entries: Vec<TableEntry>,
}

/// One row of a table. Only what the user sees is stored: never resolved
/// addresses or PIDs.
#[derive(Debug, Clone, PartialEq)]
pub struct TableEntry {
    pub description: String,
    pub address: AddrExpr,
    /// `type` in the file. `bytes` maps to `ScanType::Bytes(n)` where `n` is
    /// the length of the hex `value`.
    pub ty: ScanType,
    pub value: String,
    pub frozen: bool,
}

/// A refused table, with the 1-based line when it is known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableError {
    pub line: Option<usize>,
    pub message: String,
}

impl fmt::Display for TableError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.line {
            Some(n) => write!(f, "line {n}: {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for TableError {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTable {
    version: Option<Spanned<i64>>,
    #[serde(default)]
    process: Option<String>,
    #[serde(default, rename = "entry")]
    entries: Vec<Spanned<RawEntry>>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEntry {
    description: String,
    address: String,
    #[serde(rename = "type")]
    ty: String,
    value: String,
    #[serde(default)]
    frozen: bool,
}

/// Load and validate a table file.
pub fn load(path: &Path) -> Result<CheatTable, TableError> {
    let io_err = |e: io::Error| TableError {
        line: None,
        message: format!("{}: {e}", path.display()),
    };
    let len = std::fs::metadata(path).map_err(io_err)?.len();
    if len > MAX_FILE_BYTES {
        return Err(too_large(len));
    }
    let text = std::fs::read_to_string(path).map_err(io_err)?;
    parse(&text)
}

fn too_large(len: u64) -> TableError {
    TableError {
        line: None,
        message: format!("file too large ({len} bytes, max {MAX_FILE_BYTES})"),
    }
}

/// 1-based line of a byte offset.
fn line_of(text: &str, offset: usize) -> usize {
    let end = offset.min(text.len());
    text.as_bytes()[..end]
        .iter()
        .filter(|&&b| b == b'\n')
        .count()
        + 1
}

/// Parse and validate table text.
pub fn parse(text: &str) -> Result<CheatTable, TableError> {
    if text.len() as u64 > MAX_FILE_BYTES {
        return Err(too_large(text.len() as u64));
    }
    let raw: RawTable = toml::from_str(text).map_err(|e| TableError {
        line: e.span().map(|s| line_of(text, s.start)),
        message: e.message().to_string(),
    })?;

    let version = raw.version.ok_or_else(|| TableError {
        line: None,
        message: "missing required key `version`".into(),
    })?;
    if *version.get_ref() != FORMAT_VERSION {
        return Err(TableError {
            line: Some(line_of(text, version.span().start)),
            message: format!("unsupported version {}", version.get_ref()),
        });
    }
    if raw.entries.len() > MAX_ENTRIES {
        return Err(TableError {
            line: None,
            message: format!(
                "too many entries ({}, max {MAX_ENTRIES})",
                raw.entries.len()
            ),
        });
    }

    let mut entries = Vec::with_capacity(raw.entries.len());
    for spanned in raw.entries {
        let line = line_of(text, spanned.span().start);
        entries.push(
            check_entry(spanned.into_inner()).map_err(|message| TableError {
                line: Some(line),
                message,
            })?,
        );
    }
    Ok(CheatTable {
        process: raw.process,
        entries,
    })
}

fn check_entry(raw: RawEntry) -> Result<TableEntry, String> {
    let address = AddrExpr::parse(&raw.address).map_err(|e| format!("address: {e}"))?;

    let is_bytes = raw.ty.trim().eq_ignore_ascii_case("bytes");
    let mut ty = if is_bytes {
        ScanType::Bytes(0)
    } else {
        ScanType::from_label(&raw.ty).ok_or_else(|| format!("unknown type {:?}", raw.ty))?
    };

    if is_bytes {
        let n = hex_len(&raw.value)
            .ok_or_else(|| format!("value {:?} is not valid hex bytes", raw.value))?;
        ty = ScanType::Bytes(n);
    } else if ty.parse(&raw.value).is_none() {
        let numeric_int = !ty.is_float() && raw.value.trim().parse::<i128>().is_ok();
        return Err(if numeric_int {
            format!("value {:?} does not fit {}", raw.value, ty.label())
        } else {
            format!("value {:?} is not a valid {}", raw.value, ty.label())
        });
    }

    Ok(TableEntry {
        description: raw.description,
        address,
        ty,
        value: raw.value,
        frozen: raw.frozen,
    })
}

/// Number of bytes in a strict space-separated hex string ("DE AD be ef"),
/// or `None` if it is empty or malformed.
fn hex_len(s: &str) -> Option<usize> {
    let mut n = 0;
    for tok in s.split_whitespace() {
        if tok.len() > 2 || !tok.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        n += 1;
    }
    (n > 0).then_some(n)
}

/// Serialize a table. Always writes `ScanType::label` for the type.
pub fn to_toml(table: &CheatTable) -> String {
    #[derive(Serialize)]
    struct Out<'a> {
        version: i64,
        #[serde(skip_serializing_if = "Option::is_none")]
        process: &'a Option<String>,
        #[serde(rename = "entry", skip_serializing_if = "Vec::is_empty")]
        entries: Vec<RawEntry>,
    }
    let out = Out {
        version: FORMAT_VERSION,
        process: &table.process,
        entries: table
            .entries
            .iter()
            .map(|e| RawEntry {
                description: e.description.clone(),
                address: e.address.to_string(),
                ty: e.ty.label().to_string(),
                value: e.value.clone(),
                frozen: e.frozen,
            })
            .collect(),
    };
    toml::to_string(&out).expect("a cheat table always serializes")
}

/// Save a table by writing `<path>.tmp-<pid>` and renaming it over `path`.
pub fn save(table: &CheatTable, path: &Path) -> io::Result<()> {
    let mut tmp = OsString::from(path.as_os_str());
    tmp.push(format!(".tmp-{}", std::process::id()));
    let tmp = std::path::PathBuf::from(tmp);
    let result = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(to_toml(table).as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}
