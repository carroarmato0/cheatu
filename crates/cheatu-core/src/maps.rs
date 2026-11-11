//! A process's memory map.

use std::io;

/// A single mapped memory region of a process.
#[derive(Clone, Debug)]
pub struct MemoryRegion {
    pub start: u64,
    pub end: u64,
    pub read: bool,
    pub write: bool,
    pub exec: bool,
    pub shared: bool,
    /// Pathname / pseudo-name, e.g. `[heap]`, `[stack]`, or a library path.
    pub path: String,
}

impl MemoryRegion {
    pub fn size(&self) -> u64 {
        self.end.saturating_sub(self.start)
    }

    /// Whether this region is a sensible target for value scanning: readable,
    /// writable, and not one of the special kernel maps that either can't be
    /// safely read (`[vvar]`, `[vsyscall]`) or never hold game state.
    pub fn is_scannable(&self) -> bool {
        if !(self.read && self.write) {
            return false;
        }
        !matches!(self.path.as_str(), "[vvar]" | "[vdso]" | "[vsyscall]")
    }

    /// Classify the region for the "real value" hint. Real game state usually
    /// lives on the heap or in a module's writable data/bss; the stack holds
    /// transient locals and copies.
    pub fn kind(&self) -> RegionKind {
        match self.path.as_str() {
            "[heap]" => RegionKind::Heap,
            "[stack]" => RegionKind::Stack,
            "" => RegionKind::Anonymous,
            // A backed pathname (a library / executable). Its writable,
            // non-executable segment is the module's data/bss — where globals
            // and singletons like player stats live. Other `[...]` pseudo-names
            // (e.g. `[stack:tid]`, mapped files' text) fall through to Other.
            p if !p.starts_with('[') && self.write && !self.exec => RegionKind::ModuleData,
            _ => RegionKind::Other,
        }
    }
}

/// Coarse classification of a memory region, used to rank scan candidates by
/// how likely they are to be the authoritative game value (see
/// `scan::address_hint`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RegionKind {
    Heap,
    Stack,
    ModuleData,
    Anonymous,
    Other,
}

/// Find the region containing `addr`, if any.
///
// ponytail: linear scan. `/proc/pid/maps` is address-sorted so a binary search
// is possible, but the hint only runs on a narrowed candidate set (<= a couple
// hundred rows) against a few hundred regions — linear is plenty. Switch to
// binary if it ever shows up in a profile.
pub fn region_for(regions: &[MemoryRegion], addr: u64) -> Option<&MemoryRegion> {
    regions.iter().find(|r| addr >= r.start && addr < r.end)
}

/// Read the memory map of `pid`, sorted by address (`/proc/<pid>/maps` on
/// Linux, the Mach VM region list on macOS).
pub fn read_maps(pid: i32) -> io::Result<Vec<MemoryRegion>> {
    crate::sys::read_maps(pid)
}

/// The module (image) name of a region: the last component of its path, for
/// regions backed by a file. Anonymous regions and pseudo-names such as
/// `[heap]` have none.
pub fn module_name(region: &MemoryRegion) -> Option<&str> {
    if region.path.is_empty() || region.path.starts_with('[') {
        return None;
    }
    region.path.rsplit('/').next()
}

/// Why a module name could not be turned into a base address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModuleError {
    /// No loaded image has this file name.
    NotFound(String),
    /// Several different images share this file name.
    Ambiguous { name: String, paths: Vec<String> },
}

impl std::fmt::Display for ModuleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModuleError::NotFound(name) => write!(f, "module \"{name}\" not found"),
            ModuleError::Ambiguous { name, paths } => {
                write!(f, "module \"{name}\" is ambiguous: {}", paths.join(", "))
            }
        }
    }
}

impl std::error::Error for ModuleError {}

/// Base address of the image called `name`: the lowest start address of its
/// regions. Fails when no image, or more than one distinct image, has that
/// file name.
pub fn module_base(regions: &[MemoryRegion], name: &str) -> Result<u64, ModuleError> {
    let mut paths: Vec<String> = Vec::new();
    let mut base: Option<u64> = None;
    for r in regions.iter().filter(|r| module_name(r) == Some(name)) {
        if !paths.contains(&r.path) {
            paths.push(r.path.clone());
        }
        base = Some(base.map_or(r.start, |b| b.min(r.start)));
    }
    match (base, paths.len()) {
        (None, _) => Err(ModuleError::NotFound(name.to_string())),
        (Some(b), 1) => Ok(b),
        _ => {
            paths.sort();
            Err(ModuleError::Ambiguous {
                name: name.to_string(),
                paths,
            })
        }
    }
}

/// The image containing `addr` as `(name, base)`, or `None` for anonymous
/// memory and pseudo-regions.
pub fn module_containing(regions: &[MemoryRegion], addr: u64) -> Option<(String, u64)> {
    let region = region_for(regions, addr)?;
    let name = module_name(region)?;
    let base = regions
        .iter()
        .filter(|r| r.path == region.path)
        .map(|r| r.start)
        .min()?;
    Some((name.to_string(), base))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region(start: u64, end: u64, path: &str) -> MemoryRegion {
        MemoryRegion {
            start,
            end,
            read: true,
            write: false,
            exec: false,
            shared: false,
            path: path.to_string(),
        }
    }

    #[test]
    fn module_base_picks_lowest_start() {
        let regions = vec![
            region(0x2000, 0x3000, "/a/dmhost"),
            region(0x1000, 0x2000, "/a/dmhost"),
            region(0x9000, 0xa000, "[heap]"),
        ];
        assert_eq!(module_base(&regions, "dmhost").unwrap(), 0x1000);
    }

    #[test]
    fn module_base_ambiguous_names_both_paths() {
        let regions = vec![
            region(0x1000, 0x2000, "/a/libfoo.dylib"),
            region(0x5000, 0x6000, "/b/libfoo.dylib"),
        ];
        match module_base(&regions, "libfoo.dylib") {
            Err(ModuleError::Ambiguous { name, paths }) => {
                assert_eq!(name, "libfoo.dylib");
                assert_eq!(paths, vec!["/a/libfoo.dylib", "/b/libfoo.dylib"]);
            }
            other => panic!("unexpected: {other:?}"),
        }
        let msg = module_base(&regions, "libfoo.dylib")
            .unwrap_err()
            .to_string();
        assert_eq!(
            msg,
            "module \"libfoo.dylib\" is ambiguous: /a/libfoo.dylib, /b/libfoo.dylib"
        );
    }

    #[test]
    fn module_base_unknown() {
        let regions = vec![region(0x1000, 0x2000, "/a/dmhost"), region(0, 1, "")];
        let err = module_base(&regions, "nope").unwrap_err();
        assert_eq!(err, ModuleError::NotFound("nope".to_string()));
        assert_eq!(err.to_string(), "module \"nope\" not found");
    }

    #[test]
    fn module_name_skips_anonymous_and_pseudo() {
        assert_eq!(
            module_name(&region(0, 1, "/usr/lib/libc.so.6")),
            Some("libc.so.6")
        );
        assert_eq!(module_name(&region(0, 1, "")), None);
        assert_eq!(module_name(&region(0, 1, "[heap]")), None);
    }

    #[test]
    fn module_containing_returns_name_and_base() {
        let regions = vec![
            region(0x1000, 0x2000, "/a/dmhost"),
            region(0x2000, 0x3000, "/a/dmhost"),
            region(0x9000, 0xa000, "[heap]"),
        ];
        assert_eq!(
            module_containing(&regions, 0x2500),
            Some(("dmhost".to_string(), 0x1000))
        );
        assert_eq!(module_containing(&regions, 0x9500), None);
        assert_eq!(module_containing(&regions, 0x7000), None);
    }
}
