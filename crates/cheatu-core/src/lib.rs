//! Core memory-scanning engine for cheatu.
//!
//! This crate is UI-agnostic. It knows how to:
//!   * enumerate running processes ([`process`])
//!   * parse a process's memory map ([`maps`])
//!   * read and write another process's memory ([`mem`])
//!   * represent typed scan values ([`value`])
//!   * run first/next scans and keep candidate addresses ([`scan`])
//!   * build a disk-backed pointer map for pointer scans ([`pointer_scan`])
//!   * check for / request elevated privileges ([`privilege`])
//!
//! On Linux, reading another process's memory requires either running as root
//! or holding `CAP_SYS_PTRACE` (and a permissive `yama/ptrace_scope`); on macOS
//! it requires root. The [`privilege`] module helps relaunch the tool elevated
//! (`pkexec` on Linux, the admin prompt on macOS) when needed.

pub mod addr_expr;
pub mod maps;
pub mod mem;
pub mod pointer_scan;
pub mod privilege;
pub mod process;
pub mod scan;
pub mod signature;
pub mod source;
mod sys;
pub mod table;
pub mod value;

pub use addr_expr::{AddrExpr, Expr, ParseError, ResolveError};
pub use maps::{
    image_path, module_base, module_containing, module_name, read_maps, region_for, MemoryRegion,
    ModuleError, RegionKind,
};
pub use mem::{probe_address, Mem, ProbeOutcome};
pub use process::{human_bytes, list_processes, resume, suspend, ProcInfo};
pub use scan::{
    address_hint, parse_aob, parse_range, AddrHint, Confidence, FirstScan, NextScan, ScanProgress,
    Scanner, ANY_TYPES,
};
pub use signature::{make_signature, SignatureError};
pub use source::{LiveSource, MemSource};
pub use value::{ScanType, ScanValue};
