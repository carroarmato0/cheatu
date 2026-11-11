//! Saved pointer scan results (`.cheatu-ptr`) and the filters that narrow them
//! down after the target restarts.
//!
//! File layout, little-endian:
//!
//! ```text
//! b"CHEATUPTR"  u32 version (1)  u32 header length  JSON header
//! per chain:    u32 module index  u64 base offset  u8 depth  depth × u64 offset
//! ```
//!
//! The header holds `process`, `target`, `settings`, `modules` and
//! `truncated`. Result files are meant to be shared, so loading treats them as
//! untrusted: every length and index is checked before use and anything
//! malformed is [`io::ErrorKind::InvalidData`].

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::{PointerChain, PtrScanSettings};
use crate::source::MemSource;
use crate::value::{ScanType, ScanValue};

const MAGIC: &[u8; 9] = b"CHEATUPTR";
const VERSION: u32 = 1;
/// Largest accepted JSON header.
const MAX_HEADER: u32 = 1024 * 1024;

/// The chains found by a pointer scan, with what is needed to reuse them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PtrResults {
    /// Name of the scanned process.
    pub process: String,
    /// The address the scan searched for.
    pub target: u64,
    pub settings: PtrScanSettings,
    /// Shortest first.
    pub chains: Vec<PointerChain>,
    /// The search hit a cap, so chains may be missing.
    pub truncated: bool,
}

#[derive(Serialize, Deserialize)]
struct Header {
    process: String,
    target: u64,
    settings: PtrScanSettings,
    modules: Vec<String>,
    #[serde(default)]
    truncated: bool,
}

fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

/// Read exactly `buf.len()` bytes; a short read is [`io::ErrorKind::InvalidData`].
fn read_exact(r: &mut impl Read, buf: &mut [u8]) -> io::Result<()> {
    r.read_exact(buf).map_err(|e| {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            invalid("pointer results file is truncated")
        } else {
            e
        }
    })
}

fn read_u64(r: &mut impl Read) -> io::Result<u64> {
    let mut b = [0u8; 8];
    read_exact(r, &mut b)?;
    Ok(u64::from_le_bytes(b))
}

/// The first `u32` of a chain record, or `None` at a clean end of file.
fn read_record_start(r: &mut impl Read) -> io::Result<Option<u32>> {
    let mut b = [0u8; 4];
    let mut got = 0;
    while got < b.len() {
        match r.read(&mut b[got..]) {
            Ok(0) if got == 0 => return Ok(None),
            Ok(0) => return Err(invalid("pointer results file is truncated")),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(Some(u32::from_le_bytes(b)))
}

impl PtrResults {
    /// Write the results to `path`, replacing it.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let mut modules: Vec<String> = Vec::new();
        let mut index: HashMap<&str, u32> = HashMap::new();
        for c in &self.chains {
            if !index.contains_key(c.module.as_str()) {
                let i = u32::try_from(modules.len())
                    .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "too many modules"))?;
                index.insert(&c.module, i);
                modules.push(c.module.clone());
            }
        }
        let header = serde_json::to_vec(&Header {
            process: self.process.clone(),
            target: self.target,
            settings: self.settings.clone(),
            modules,
            truncated: self.truncated,
        })
        .map_err(io::Error::other)?;
        let header_len = u32::try_from(header.len())
            .ok()
            .filter(|&n| n <= MAX_HEADER)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "header too large"))?;

        // Refuse what `load` would reject, before touching the file.
        let max_depth = self.settings.max_depth.min(u8::MAX as usize);
        if let Some(c) = self
            .chains
            .iter()
            .find(|c| c.offsets.is_empty() || c.offsets.len() > max_depth)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "chain depth {} out of range 1..={max_depth}",
                    c.offsets.len()
                ),
            ));
        }

        let mut w = BufWriter::new(File::create(path)?);
        w.write_all(MAGIC)?;
        w.write_all(&VERSION.to_le_bytes())?;
        w.write_all(&header_len.to_le_bytes())?;
        w.write_all(&header)?;
        for c in &self.chains {
            let depth = c.offsets.len() as u8; // checked above
            w.write_all(&index[c.module.as_str()].to_le_bytes())?;
            w.write_all(&c.base_offset.to_le_bytes())?;
            w.write_all(&[depth])?;
            for o in &c.offsets {
                w.write_all(&o.to_le_bytes())?;
            }
        }
        w.into_inner().map_err(|e| e.into_error())?.sync_all()
    }

    /// Read results written by [`save`](Self::save).
    pub fn load(path: &Path) -> io::Result<PtrResults> {
        let mut r = BufReader::new(File::open(path)?);
        let mut magic = [0u8; 9];
        read_exact(&mut r, &mut magic)?;
        if &magic != MAGIC {
            return Err(invalid("not a cheatu pointer results file"));
        }
        let mut b = [0u8; 4];
        read_exact(&mut r, &mut b)?;
        let version = u32::from_le_bytes(b);
        if version != VERSION {
            return Err(invalid(format!(
                "unsupported pointer results version {version}"
            )));
        }
        read_exact(&mut r, &mut b)?;
        let header_len = u32::from_le_bytes(b);
        if header_len > MAX_HEADER {
            return Err(invalid("pointer results header too large"));
        }
        let mut header = vec![0u8; header_len as usize];
        read_exact(&mut r, &mut header)?;
        let header: Header = serde_json::from_slice(&header)
            .map_err(|e| invalid(format!("bad pointer results header: {e}")))?;
        let max_depth = header.settings.max_depth.min(u8::MAX as usize);

        let mut chains = Vec::new();
        while let Some(module) = read_record_start(&mut r)? {
            let module = header
                .modules
                .get(module as usize)
                .ok_or_else(|| invalid(format!("module index {module} out of range")))?;
            let base_offset = read_u64(&mut r)?;
            let mut d = [0u8; 1];
            read_exact(&mut r, &mut d)?;
            let depth = d[0] as usize;
            if depth == 0 || depth > max_depth {
                return Err(invalid(format!("chain depth {depth} out of range")));
            }
            let offsets = (0..depth)
                .map(|_| read_u64(&mut r))
                .collect::<io::Result<Vec<u64>>>()?;
            chains.push(PointerChain {
                module: module.clone(),
                base_offset,
                offsets,
            });
        }
        Ok(PtrResults {
            process: header.process,
            target: header.target,
            settings: header.settings,
            chains,
            truncated: header.truncated,
        })
    }

    /// Keep the chains that resolve exactly to `addr` in `src`. Returns how
    /// many remain.
    pub fn filter_by_address(&mut self, src: &dyn MemSource, addr: u64) -> usize {
        self.chains.retain(|c| c.resolve(src) == Ok(addr));
        self.chains.len()
    }

    /// Keep the chains whose target currently holds `value` as `ty` (floats
    /// compare with the usual scan tolerance). Returns how many remain.
    pub fn filter_by_value(
        &mut self,
        src: &dyn MemSource,
        ty: ScanType,
        value: &ScanValue,
    ) -> usize {
        let mut buf = vec![0u8; ty.size()];
        self.chains.retain(|c| {
            let Ok(addr) = c.resolve(src) else {
                return false;
            };
            matches!(src.read(addr, &mut buf), Ok(n) if n == buf.len())
                && ScanValue::from_ne_bytes(ty, &buf).is_some_and(|v| v.approx_eq(value))
        });
        self.chains.len()
    }
}
