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
        })
    }
}

impl fmt::Display for ScanType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// A concrete value read from (or to be written to) memory.
#[derive(Copy, Clone, Debug)]
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
        }
    }

    /// Equality suitable for "exact"/"unchanged"/"changed" comparisons.
    ///
    /// Integers compare exactly; floats compare with a small tolerance so that
    /// e.g. a displayed `100` matches an in-memory `99.99998`.
    pub fn approx_eq(&self, other: &ScanValue) -> bool {
        match (self, other) {
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
        }
    }
}
