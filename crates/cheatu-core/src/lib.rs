//! Core memory-scanning engine for cheatu.
//!
//! This crate is UI-agnostic. It knows how to:
//!   * enumerate running processes ([`process`])
//!   * parse a process's memory map ([`maps`])
//!   * read and write another process's memory ([`mem`])
//!   * represent typed scan values ([`value`])
//!   * run first/next scans and keep candidate addresses ([`scan`])
//!   * check for / request elevated privileges ([`privilege`])
//!
//! On Linux, reading another process's memory requires either running as root
//! or holding `CAP_SYS_PTRACE` (and a permissive `yama/ptrace_scope`). The
//! [`privilege`] module helps relaunch the tool via `pkexec` when needed.

pub mod maps;
pub mod mem;
pub mod privilege;
pub mod process;
pub mod scan;
pub mod value;

pub use maps::{read_maps, region_for, MemoryRegion, RegionKind};
pub use mem::{probe_address, Mem, ProbeOutcome};
pub use process::{human_bytes, list_processes, resume, suspend, ProcInfo};
pub use scan::{
    address_hint, parse_aob, AddrHint, Confidence, FirstScan, NextScan, Scanner, ANY_TYPES,
};
pub use value::{ScanType, ScanValue};
