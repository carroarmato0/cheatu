//! Typed scan values.

use std::fmt;

/// The numeric type the scanner interprets memory as.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum ScanType {
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    I64,
    U64,
    F32,
    F64,
    /// A raw byte match of a given length — the result of an Array-of-Bytes
    /// or String search. Not user-selectable directly (see `FirstScan::Pattern`).
    Bytes(usize),
}

impl Default for ScanType {
    /// A 32-bit signed integer — the most common game value width.
    fn default() -> Self {
        ScanType::I32
    }
}

impl ScanType {
    /// Every supported type, in a sensible display order.
    pub const ALL: [ScanType; 10] = [
        ScanType::I8,
        ScanType::U8,
        ScanType::I16,
        ScanType::U16,
        ScanType::I32,
        ScanType::U32,
        ScanType::I64,
        ScanType::U64,
        ScanType::F32,
        ScanType::F64,
    ];

    /// Width in bytes.
    pub fn size(self) -> usize {
        match self {
            ScanType::I8 | ScanType::U8 => 1,
            ScanType::I16 | ScanType::U16 => 2,
            ScanType::I32 | ScanType::U32 | ScanType::F32 => 4,
            ScanType::I64 | ScanType::U64 | ScanType::F64 => 8,
            ScanType::Bytes(n) => n,
        }
    }

    /// Short human label, e.g. `"i32"`.
    pub fn label(self) -> &'static str {
        match self {
            ScanType::I8 => "i8",
            ScanType::U8 => "u8",
            ScanType::I16 => "i16",
            ScanType::U16 => "u16",
            ScanType::I32 => "i32",
            ScanType::U32 => "u32",
            ScanType::I64 => "i64",
            ScanType::U64 => "u64",
            ScanType::F32 => "f32",
            ScanType::F64 => "f64",
            ScanType::Bytes(_) => "bytes",
        }
    }

    /// One representative type per byte-width group, for UI display — pick
    /// the actual signedness with [`ScanType::with_sign`].
    pub const SIZE_GROUPS: [ScanType; 6] = [
        ScanType::I8,
        ScanType::I16,
        ScanType::I32,
        ScanType::I64,
        ScanType::F32,
        ScanType::F64,
    ];

    pub fn is_float(self) -> bool {
        matches!(self, ScanType::F32 | ScanType::F64)
    }

    pub fn is_signed(self) -> bool {
        matches!(self, ScanType::I8 | ScanType::I16 | ScanType::I32 | ScanType::I64)
    }

    /// The signed or unsigned variant at this same byte width. A no-op for
    /// floats and byte patterns, which have no sign.
    pub fn with_sign(self, signed: bool) -> ScanType {
        match self {
            ScanType::I8 | ScanType::U8 => {
                if signed {
                    ScanType::I8
                } else {
                    ScanType::U8
                }
            }
            ScanType::I16 | ScanType::U16 => {
                if signed {
                    ScanType::I16
                } else {
                    ScanType::U16
                }
            }
            ScanType::I32 | ScanType::U32 => {
                if signed {
                    ScanType::I32
                } else {
                    ScanType::U32
                }
            }
            ScanType::I64 | ScanType::U64 => {
                if signed {
                    ScanType::I64
                } else {
                    ScanType::U64
                }
            }
            other => other,
        }
    }

    /// Beginner-friendly name for the value-type picker, e.g. `"4 Bytes"`,
    /// matching Cheat Engine's terminology instead of Rust type names.
    pub fn friendly_label(self) -> String {
        match self {
            ScanType::I8 | ScanType::U8 => "Byte".to_string(),
            ScanType::I16 | ScanType::U16 => "2 Bytes".to_string(),
            ScanType::I32 | ScanType::U32 => "4 Bytes".to_string(),
            ScanType::I64 | ScanType::U64 => "8 Bytes".to_string(),
            ScanType::F32 => "Float".to_string(),
            ScanType::F64 => "Double".to_string(),
            ScanType::Bytes(n) => format!("Array of Bytes ({n})"),
        }
    }

    /// Parse a label such as `"i32"` / `"int"` / `"float"` back into a type.
    pub fn from_label(s: &str) -> Option<ScanType> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "i8" | "int8" | "sbyte" => ScanType::I8,
            "u8" | "uint8" | "byte" => ScanType::U8,
            "i16" | "int16" | "short" => ScanType::I16,
            "u16" | "uint16" | "ushort" => ScanType::U16,
            "i32" | "int32" | "int" => ScanType::I32,
            "u32" | "uint32" | "uint" => ScanType::U32,
            "i64" | "int64" | "long" => ScanType::I64,
            "u64" | "uint64" | "ulong" => ScanType::U64,
            "f32" | "float" => ScanType::F32,
            "f64" | "double" => ScanType::F64,
            _ => return None,
        })
    }

    /// Parse a user-entered value string into a [`ScanValue`] of this type.
    pub fn parse(self, s: &str) -> Option<ScanValue> {
        let s = s.trim();
        Some(match self {
            ScanType::I8 => ScanValue::I8(s.parse().ok()?),
            ScanType::U8 => ScanValue::U8(s.parse().ok()?),
            ScanType::I16 => ScanValue::I16(s.parse().ok()?),
            ScanType::U16 => ScanValue::U16(s.parse().ok()?),
            ScanType::I32 => ScanValue::I32(s.parse().ok()?),
            ScanType::U32 => ScanValue::U32(s.parse().ok()?),
            ScanType::I64 => ScanValue::I64(s.parse().ok()?),
            ScanType::U64 => ScanValue::U64(s.parse().ok()?),
            ScanType::F32 => ScanValue::F32(s.parse().ok()?),
            ScanType::F64 => ScanValue::F64(s.parse().ok()?),
            // Space-separated hex ("48 65 6C 6C 6F") if it parses as such,
            // otherwise the literal text as UTF-8 bytes.
            ScanType::Bytes(_) => ScanValue::Bytes(parse_hex_bytes(s).unwrap_or_else(|| s.as_bytes().to_vec())),
        })
    }
}

/// Strict space-separated hex byte parse, e.g. `"48 65 6C"` -> `[0x48, 0x65, 0x6C]`.
fn parse_hex_bytes(s: &str) -> Option<Vec<u8>> {
    let bytes: Option<Vec<u8>> = s.split_whitespace().map(|t| u8::from_str_radix(t, 16).ok()).collect();
    bytes.filter(|b| !b.is_empty())
}

impl fmt::Display for ScanType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// A concrete value read from (or to be written to) memory.
#[derive(Clone, Debug)]
pub enum ScanValue {
    I8(i8),
    U8(u8),
    I16(i16),
    U16(u16),
    I32(i32),
    U32(u32),
    I64(i64),
    U64(u64),
    F32(f32),
    F64(f64),
    /// A raw byte match (Array-of-Bytes / String result).
    Bytes(Vec<u8>),
}

impl ScanValue {
    pub fn ty(&self) -> ScanType {
        match self {
            ScanValue::I8(_) => ScanType::I8,
            ScanValue::U8(_) => ScanType::U8,
            ScanValue::I16(_) => ScanType::I16,
            ScanValue::U16(_) => ScanType::U16,
            ScanValue::I32(_) => ScanType::I32,
            ScanValue::U32(_) => ScanType::U32,
            ScanValue::I64(_) => ScanType::I64,
            ScanValue::U64(_) => ScanType::U64,
            ScanValue::F32(_) => ScanType::F32,
            ScanValue::F64(_) => ScanType::F64,
            ScanValue::Bytes(v) => ScanType::Bytes(v.len()),
        }
    }

    /// Decode a value of `ty` from a native-endian byte slice.
    ///
    /// Returns `None` if `bytes` is shorter than the type width.
    pub fn from_ne_bytes(ty: ScanType, bytes: &[u8]) -> Option<ScanValue> {
        let n = ty.size();
        if bytes.len() < n {
            return None;
        }
        let b = &bytes[..n];
        Some(match ty {
            ScanType::I8 => ScanValue::I8(b[0] as i8),
            ScanType::U8 => ScanValue::U8(b[0]),
            ScanType::I16 => ScanValue::I16(i16::from_ne_bytes(b.try_into().ok()?)),
            ScanType::U16 => ScanValue::U16(u16::from_ne_bytes(b.try_into().ok()?)),
            ScanType::I32 => ScanValue::I32(i32::from_ne_bytes(b.try_into().ok()?)),
            ScanType::U32 => ScanValue::U32(u32::from_ne_bytes(b.try_into().ok()?)),
            ScanType::I64 => ScanValue::I64(i64::from_ne_bytes(b.try_into().ok()?)),
            ScanType::U64 => ScanValue::U64(u64::from_ne_bytes(b.try_into().ok()?)),
            ScanType::F32 => ScanValue::F32(f32::from_ne_bytes(b.try_into().ok()?)),
            ScanType::F64 => ScanValue::F64(f64::from_ne_bytes(b.try_into().ok()?)),
            ScanType::Bytes(_) => ScanValue::Bytes(b.to_vec()),
        })
    }

    /// Native-endian byte encoding, ready to write into memory.
    pub fn to_ne_bytes(&self) -> Vec<u8> {
        match self {
            ScanValue::I8(v) => v.to_ne_bytes().to_vec(),
            ScanValue::U8(v) => v.to_ne_bytes().to_vec(),
            ScanValue::I16(v) => v.to_ne_bytes().to_vec(),
            ScanValue::U16(v) => v.to_ne_bytes().to_vec(),
            ScanValue::I32(v) => v.to_ne_bytes().to_vec(),
            ScanValue::U32(v) => v.to_ne_bytes().to_vec(),
            ScanValue::I64(v) => v.to_ne_bytes().to_vec(),
            ScanValue::U64(v) => v.to_ne_bytes().to_vec(),
            ScanValue::F32(v) => v.to_ne_bytes().to_vec(),
            ScanValue::F64(v) => v.to_ne_bytes().to_vec(),
            ScanValue::Bytes(v) => v.clone(),
        }
    }

    /// Value as `f64`, used for ordering (`>`, `<`) and inc/dec comparisons.
    pub fn as_f64(&self) -> f64 {
        match self {
            ScanValue::I8(v) => *v as f64,
            ScanValue::U8(v) => *v as f64,
            ScanValue::I16(v) => *v as f64,
            ScanValue::U16(v) => *v as f64,
            ScanValue::I32(v) => *v as f64,
            ScanValue::U32(v) => *v as f64,
            ScanValue::I64(v) => *v as f64,
            ScanValue::U64(v) => *v as f64,
            ScanValue::F32(v) => *v as f64,
            ScanValue::F64(v) => *v,
            // No meaningful ordering — this makes Gt/Lt/Increased/Decreased
            // naturally match nothing rather than compare byte content.
            ScanValue::Bytes(_) => f64::NAN,
        }
    }

    /// Integer value as `i128` (only meaningful for integer variants).
    fn as_i128(&self) -> i128 {
        match self {
            ScanValue::I8(v) => *v as i128,
            ScanValue::U8(v) => *v as i128,
            ScanValue::I16(v) => *v as i128,
            ScanValue::U16(v) => *v as i128,
            ScanValue::I32(v) => *v as i128,
            ScanValue::U32(v) => *v as i128,
            ScanValue::I64(v) => *v as i128,
            ScanValue::U64(v) => *v as i128,
            ScanValue::F32(v) => *v as i128,
            ScanValue::F64(v) => *v as i128,
            ScanValue::Bytes(_) => 0, // unreachable: approx_eq handles Bytes separately
        }
    }

    /// Equality suitable for "exact"/"unchanged"/"changed" comparisons.
    ///
    /// Integers compare exactly; floats compare with a small tolerance so that
    /// e.g. a displayed `100` matches an in-memory `99.99998`; byte matches
    /// compare exactly.
    pub fn approx_eq(&self, other: &ScanValue) -> bool {
        match (self, other) {
            (ScanValue::Bytes(a), ScanValue::Bytes(b)) => a == b,
            (ScanValue::F32(_), _) | (ScanValue::F64(_), _) => {
                let (a, b) = (self.as_f64(), other.as_f64());
                (a - b).abs() <= 1e-4_f64.max(b.abs() * 1e-6)
            }
            _ => self.as_i128() == other.as_i128(),
        }
    }
}

impl fmt::Display for ScanValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ScanValue::I8(v) => write!(f, "{v}"),
            ScanValue::U8(v) => write!(f, "{v}"),
            ScanValue::I16(v) => write!(f, "{v}"),
            ScanValue::U16(v) => write!(f, "{v}"),
            ScanValue::I32(v) => write!(f, "{v}"),
            ScanValue::U32(v) => write!(f, "{v}"),
            ScanValue::I64(v) => write!(f, "{v}"),
            ScanValue::U64(v) => write!(f, "{v}"),
            ScanValue::F32(v) => write!(f, "{v}"),
            ScanValue::F64(v) => write!(f, "{v}"),
            // Show matched text when it's valid UTF-8 (the common case for a
            // String search), otherwise fall back to hex for raw AoB matches.
            ScanValue::Bytes(v) => match std::str::from_utf8(v) {
                Ok(s) if !s.contains('\0') => write!(f, "{s}"),
                _ => write!(
                    f,
                    "{}",
                    v.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" ")
                ),
            },
        }
    }
}
