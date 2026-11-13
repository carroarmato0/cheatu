//! macOS backend: Mach VM calls on the target's task port, libproc and
//! `sysctl` for process information, and the administrator prompt (osascript)
//! for elevation.
//!
//! `task_for_pid` on another process needs root, and even root is refused for
//! targets protected by the hardened runtime or System Integrity Protection.
//! Our own task is always accessible, which is what the tests rely on.

use std::ffi::CStr;
use std::io;
use std::path::Path;

use mach2::kern_return::{
    kern_return_t, KERN_INVALID_ADDRESS, KERN_PROTECTION_FAILURE, KERN_SUCCESS,
};
use mach2::mach_port::mach_port_deallocate;
use mach2::port::{mach_port_name_t, mach_port_t, MACH_PORT_NULL};
use mach2::traps::{mach_task_self, task_for_pid};
use mach2::vm::{
    mach_vm_protect, mach_vm_read_overwrite, mach_vm_region, mach_vm_region_recurse, mach_vm_write,
};
use mach2::vm_prot::{vm_prot_t, VM_PROT_COPY, VM_PROT_EXECUTE, VM_PROT_READ, VM_PROT_WRITE};
use mach2::vm_region::{
    vm_region_basic_info_64, vm_region_info_t, vm_region_recurse_info_t, vm_region_submap_info_64,
    SM_SHARED, SM_SHARED_ALIASED, SM_TRUESHARED, VM_REGION_BASIC_INFO_64,
};
use mach2::vm_types::{mach_vm_address_t, mach_vm_size_t, natural_t};

use super::RawProc;
use crate::maps::MemoryRegion;

extern "C" {
    // Declared here: libc's binding is deprecated and mach2 0.4 lacks one.
    fn mach_host_self() -> mach_port_t;
    fn mach_error_string(error_value: kern_return_t) -> *const libc::c_char;
}

/// `user_tag`s of anonymous regions, from `<mach/vm_statistics.h>`.
const VM_MEMORY_MALLOC_FIRST: u32 = 1; // VM_MEMORY_MALLOC
const VM_MEMORY_MALLOC_LAST: u32 = 12; // VM_MEMORY_MALLOC_MEDIUM
const VM_MEMORY_STACK: u32 = 30;

fn mach_error(kr: kern_return_t) -> io::Error {
    // SAFETY: mach_error_string returns a pointer to a static C string.
    let msg = unsafe { CStr::from_ptr(mach_error_string(kr)) };
    io::Error::other(format!("{} (kern_return_t {kr})", msg.to_string_lossy()))
}

/// The error for a `task_for_pid` refusal on a process that exists.
fn denied_error(pid: i32, as_root: bool) -> io::Error {
    let msg = if as_root {
        format!(
            "macOS refused access to process {pid} even as root — it is most likely \
             protected by the hardened runtime or System Integrity Protection. Re-sign \
             the game ad-hoc without the hardened runtime (codesign --force --sign - <app>) \
             or disable SIP to scan it."
        )
    } else {
        format!("permission denied for process {pid}: run cheatu as root (elevate) to scan it")
    };
    io::Error::new(io::ErrorKind::PermissionDenied, msg)
}

fn process_exists(pid: i32) -> bool {
    // SAFETY: signal 0 only checks for existence and permission.
    let rc = unsafe { libc::kill(pid, 0) };
    rc == 0 || io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

fn is_root() -> bool {
    // SAFETY: geteuid is always safe to call.
    unsafe { libc::geteuid() == 0 }
}

/// A send right to a target's task port. Plain integer, so `Send + Sync`;
/// Mach VM calls on it are thread-safe.
pub(crate) struct MemHandle {
    task: mach_port_name_t,
}

impl MemHandle {
    pub(crate) fn open(pid: i32) -> io::Result<MemHandle> {
        if pid <= 0 || !process_exists(pid) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("no such process: {pid}"),
            ));
        }
        let mut task: mach_port_name_t = MACH_PORT_NULL;
        // SAFETY: task_for_pid writes a port name into `task` on success.
        let kr = unsafe { task_for_pid(mach_task_self(), pid, &mut task) };
        if kr != KERN_SUCCESS {
            return Err(denied_error(pid, is_root()));
        }
        Ok(MemHandle { task })
    }

    pub(crate) fn read_at(&self, addr: u64, buf: &mut [u8]) -> io::Result<usize> {
        let mut read: mach_vm_size_t = 0;
        // SAFETY: the kernel copies at most buf.len() bytes into buf.
        let kr = unsafe {
            mach_vm_read_overwrite(
                self.task,
                addr,
                buf.len() as mach_vm_size_t,
                buf.as_mut_ptr() as mach_vm_address_t,
                &mut read,
            )
        };
        if kr != KERN_SUCCESS {
            return Err(mach_error(kr));
        }
        Ok(read as usize)
    }

    pub(crate) fn write_at(&self, addr: u64, buf: &[u8]) -> io::Result<()> {
        let kr = self.raw_write(addr, buf);
        if kr == KERN_SUCCESS {
            return Ok(());
        }
        // A read-only page reports KERN_PROTECTION_FAILURE or, for
        // mach_vm_write, KERN_INVALID_ADDRESS; protection_at tells the latter
        // apart from a genuinely unmapped address.
        if kr != KERN_PROTECTION_FAILURE && kr != KERN_INVALID_ADDRESS {
            return Err(mach_error(kr));
        }
        // Read-only page: make it writable (copy-on-write for shared/file
        // mappings), write, then put the original protection back.
        let prot = self.protection_at(addr)?;
        let page = page_size();
        let start = addr & !(page - 1);
        let len = (addr + buf.len() as u64).div_ceil(page) * page - start;
        // SAFETY: changes protection of pages in the target only.
        let kr = unsafe {
            mach_vm_protect(
                self.task,
                start,
                len,
                0,
                prot | VM_PROT_WRITE | VM_PROT_COPY,
            )
        };
        if kr != KERN_SUCCESS {
            return Err(mach_error(kr));
        }
        let written = self.raw_write(addr, buf);
        // SAFETY: as above.
        unsafe { mach_vm_protect(self.task, start, len, 0, prot) };
        if written != KERN_SUCCESS {
            return Err(mach_error(written));
        }
        Ok(())
    }

    fn raw_write(&self, addr: u64, buf: &[u8]) -> kern_return_t {
        // SAFETY: the kernel reads buf.len() bytes from buf.
        unsafe { mach_vm_write(self.task, addr, buf.as_ptr() as usize, buf.len() as u32) }
    }

    /// Current protection of the region containing `addr`.
    fn protection_at(&self, addr: u64) -> io::Result<vm_prot_t> {
        let mut start: mach_vm_address_t = addr;
        let mut size: mach_vm_size_t = 0;
        let mut info = vm_region_basic_info_64::default();
        let mut count = vm_region_basic_info_64::count();
        let mut object: mach_port_t = MACH_PORT_NULL;
        // SAFETY: info/count describe a correctly sized out-buffer.
        let kr = unsafe {
            mach_vm_region(
                self.task,
                &mut start,
                &mut size,
                VM_REGION_BASIC_INFO_64,
                &mut info as *mut _ as vm_region_info_t,
                &mut count,
                &mut object,
            )
        };
        if kr != KERN_SUCCESS {
            return Err(mach_error(kr));
        }
        if start > addr {
            return Err(io::Error::other(format!("address {addr:#x} is not mapped")));
        }
        Ok(info.protection)
    }
}

impl Drop for MemHandle {
    fn drop(&mut self) {
        // SAFETY: releases the send right task_for_pid gave us.
        unsafe { mach_port_deallocate(mach_task_self(), self.task) };
    }
}

pub(crate) fn accessible(pid: i32) -> bool {
    MemHandle::open(pid).is_ok()
}

/// Walk the target's VM map, descending into submaps (e.g. the dyld shared
/// cache), so the result is the flat, address-sorted list `/proc/<pid>/maps`
/// would give on Linux.
pub(crate) fn read_maps(pid: i32) -> io::Result<Vec<MemoryRegion>> {
    let handle = MemHandle::open(pid)?;
    let mut regions = Vec::new();
    let mut addr: mach_vm_address_t = 0;
    let mut depth: natural_t = 0;
    loop {
        let mut size: mach_vm_size_t = 0;
        let mut info = vm_region_submap_info_64::default();
        let mut count = vm_region_submap_info_64::count();
        // SAFETY: info/count describe a correctly sized out-buffer.
        let kr = unsafe {
            mach_vm_region_recurse(
                handle.task,
                &mut addr,
                &mut size,
                &mut depth,
                &mut info as *mut _ as vm_region_recurse_info_t,
                &mut count,
            )
        };
        if kr != KERN_SUCCESS {
            break; // KERN_INVALID_ADDRESS: past the last region
        }
        if info.is_submap != 0 {
            depth += 1;
            continue;
        }
        let prot = info.protection;
        regions.push(MemoryRegion {
            start: addr,
            end: addr + size,
            read: prot & VM_PROT_READ != 0,
            write: prot & VM_PROT_WRITE != 0,
            exec: prot & VM_PROT_EXECUTE != 0,
            shared: matches!(
                info.share_mode,
                SM_SHARED | SM_TRUESHARED | SM_SHARED_ALIASED
            ),
            path: region_path(pid, addr, info.user_tag),
        });
        addr += size;
    }
    Ok(regions)
}

/// `PROC_PIDREGIONPATHINFO` from `<sys/proc_info.h>`.
const PROC_PIDREGIONPATHINFO: libc::c_int = 8;

/// `struct proc_regioninfo` from `<sys/proc_info.h>` (libc lacks it).
#[repr(C)]
struct ProcRegionInfo {
    pri_protection: u32,
    pri_max_protection: u32,
    pri_inheritance: u32,
    pri_flags: u32,
    pri_offset: u64,
    pri_behavior: u32,
    pri_user_wired_count: u32,
    pri_user_tag: u32,
    pri_pages_resident: u32,
    pri_pages_shared_now_private: u32,
    pri_pages_swapped_out: u32,
    pri_pages_dirtied: u32,
    pri_ref_count: u32,
    pri_shadow_depth: u32,
    pri_share_mode: u32,
    pri_private_pages_resident: u32,
    pri_shared_pages_resident: u32,
    pri_obj_id: u32,
    pri_depth: u32,
    pri_address: u64,
    pri_size: u64,
}

const _: () = assert!(std::mem::size_of::<ProcRegionInfo>() == 96);

/// `struct proc_regionwithpathinfo` from `<sys/proc_info.h>`.
#[repr(C)]
struct ProcRegionWithPathInfo {
    prp_prinfo: ProcRegionInfo,
    prp_vip: libc::vnode_info_path,
}

/// The backing file of a region, or a Linux-style pseudo-name (`[heap]`,
/// `[stack]`) derived from the anonymous region's tag so `RegionKind` works
/// unchanged.
///
/// The kernel answers for the first region at *or after* `addr`, so an
/// anonymous region just below a mapped file would otherwise be reported as
/// that file (and count as a module image). `proc_regionfilename` hides the
/// region bounds, hence the direct `proc_pidinfo` call.
fn region_path(pid: i32, addr: u64, user_tag: u32) -> String {
    // SAFETY: plain-old-data struct; all-zero is a valid value.
    let mut info: ProcRegionWithPathInfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<ProcRegionWithPathInfo>() as libc::c_int;
    // SAFETY: info is `size` bytes and the flavor fills exactly this struct.
    let n = unsafe {
        libc::proc_pidinfo(
            pid,
            PROC_PIDREGIONPATHINFO,
            addr,
            (&mut info as *mut ProcRegionWithPathInfo).cast(),
            size,
        )
    };
    let r = &info.prp_prinfo;
    if n == size && r.pri_address <= addr && addr - r.pri_address < r.pri_size {
        // SAFETY: vip_path is MAXPATHLEN c_chars, NUL-terminated by the kernel
        // (and zero-initialised above).
        let raw: &[u8] = unsafe {
            std::slice::from_raw_parts(
                info.prp_vip.vip_path.as_ptr().cast(),
                std::mem::size_of_val(&info.prp_vip.vip_path),
            )
        };
        let len = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
        if len > 0 {
            return String::from_utf8_lossy(&raw[..len]).into_owned();
        }
    }
    match user_tag {
        VM_MEMORY_MALLOC_FIRST..=VM_MEMORY_MALLOC_LAST => "[heap]".into(),
        VM_MEMORY_STACK => "[stack]".into(),
        _ => String::new(),
    }
}

fn page_size() -> u64 {
    // SAFETY: sysconf is always safe to call.
    let v = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if v > 0 {
        v as u64
    } else {
        16384
    }
}

/// All processes libproc reports, unsorted. Processes with neither readable
/// arguments nor an executable path (e.g. `kernel_task`) are skipped.
/// The parent pid from `PROC_PIDTBSDINFO`, 0 when it cannot be read.
fn parent_pid(pid: i32) -> i32 {
    // SAFETY: proc_bsdinfo is plain old data; all-zero is a valid value.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as i32;
    // SAFETY: the buffer is a proc_bsdinfo of `size` bytes.
    let n = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            size,
        )
    };
    if n == size {
        info.pbi_ppid as i32
    } else {
        0
    }
}

pub(crate) fn raw_processes() -> Vec<RawProc> {
    // SAFETY: a null buffer asks for the current number of pids.
    let n = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if n <= 0 {
        return Vec::new();
    }
    // Headroom for processes started between the two calls.
    let mut pids = vec![0i32; n as usize + 64];
    // SAFETY: the buffer holds pids.len() i32s.
    let n = unsafe {
        libc::proc_listallpids(
            pids.as_mut_ptr().cast(),
            (pids.len() * std::mem::size_of::<i32>()) as i32,
        )
    };
    pids.truncate(n.max(0) as usize);

    pids.into_iter()
        .filter(|&pid| pid > 0)
        .filter_map(|pid| {
            let cmdline = match procargs(pid) {
                Some((argv, _)) if !argv.is_empty() => argv.join(" "),
                _ => pid_path(pid)?,
            };
            Some(RawProc {
                pid,
                ppid: parent_pid(pid),
                name: pid_name(pid),
                cmdline,
                rss_bytes: resident_size(pid),
            })
        })
        .collect()
}

/// The `KEY=VALUE` environment of `pid`, from `KERN_PROCARGS2`. Readable for
/// our own processes (or as root).
pub(crate) fn environ(pid: i32) -> Option<Vec<String>> {
    procargs(pid).map(|(_, env)| env)
}

/// argv and environment of `pid`, parsed from the `KERN_PROCARGS2` sysctl:
/// `argc: i32`, the exec path, NUL padding, `argc` NUL-terminated arguments,
/// then NUL-terminated `KEY=VALUE` strings up to an empty one.
fn procargs(pid: i32) -> Option<(Vec<String>, Vec<String>)> {
    let mut argmax: libc::c_int = 0;
    let mut len = std::mem::size_of::<libc::c_int>();
    let mut mib = [libc::CTL_KERN, libc::KERN_ARGMAX];
    // SAFETY: argmax/len describe a c_int out-buffer.
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            2,
            &mut argmax as *mut _ as *mut libc::c_void,
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || argmax <= 0 {
        return None;
    }
    let mut buf = vec![0u8; argmax as usize];
    let mut len = buf.len();
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    // SAFETY: buf/len describe the out-buffer.
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            buf.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || len < 4 {
        return None;
    }
    buf.truncate(len);
    let argc = i32::from_ne_bytes(buf[..4].try_into().ok()?).max(0) as usize;

    let mut fields = buf[4..].split(|&b| b == 0);
    fields.next()?; // exec path
    let mut fields = fields.skip_while(|f| f.is_empty()); // padding
    let argv: Vec<String> = fields
        .by_ref()
        .take(argc)
        .map(|f| String::from_utf8_lossy(f).into_owned())
        .collect();
    let env = fields
        .take_while(|f| !f.is_empty())
        .map(|f| String::from_utf8_lossy(f).into_owned())
        .collect();
    Some((argv, env))
}

fn pid_name(pid: i32) -> String {
    let mut buf = [0u8; 256];
    // SAFETY: buf is 256 bytes; the call writes at most that many.
    let n = unsafe { libc::proc_name(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    String::from_utf8_lossy(&buf[..n.max(0) as usize]).into_owned()
}

fn pid_path(pid: i32) -> Option<String> {
    let mut buf = [0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: buf is PROC_PIDPATHINFO_MAXSIZE bytes.
    let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    (n > 0).then(|| String::from_utf8_lossy(&buf[..n as usize]).into_owned())
}

/// Resident memory in bytes, best-effort 0.
fn resident_size(pid: i32) -> u64 {
    // SAFETY: proc_taskinfo is plain old data.
    let mut info: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_taskinfo>() as i32;
    // SAFETY: info is `size` bytes.
    let n = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTASKINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            size,
        )
    };
    if n == size {
        info.pti_resident_size
    } else {
        0
    }
}

/// Free + inactive physical memory, falling back to half of installed RAM.
pub(crate) fn available_memory() -> Option<usize> {
    let mut stats: libc::vm_statistics64 = unsafe { std::mem::zeroed() };
    let mut count = libc::HOST_VM_INFO64_COUNT;
    // SAFETY: stats/count describe a correctly sized out-buffer.
    let kr = unsafe {
        libc::host_statistics64(
            mach_host_self(),
            libc::HOST_VM_INFO64,
            &mut stats as *mut _ as libc::host_info64_t,
            &mut count,
        )
    };
    if kr == KERN_SUCCESS {
        let pages = stats.free_count as u64 + stats.inactive_count as u64;
        return Some((pages * page_size()) as usize);
    }
    let mut memsize: u64 = 0;
    let mut len = std::mem::size_of::<u64>();
    // SAFETY: memsize/len describe a u64 out-buffer for hw.memsize.
    let rc = unsafe {
        libc::sysctlbyname(
            c"hw.memsize".as_ptr(),
            &mut memsize as *mut u64 as *mut libc::c_void,
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    (rc == 0).then_some((memsize / 2) as usize)
}

pub(crate) const ELEVATION_TOOL: &str = "the macOS admin prompt";

const OSASCRIPT: &str = "/usr/bin/osascript";

pub(crate) fn elevation_available() -> bool {
    Path::new(OSASCRIPT).is_file()
}

/// Relaunch `exe` with `args` as root through the native administrator
/// prompt. On success the elevated instance runs in the background and this
/// process exits; only returns on failure (including the user cancelling).
pub(crate) fn relaunch_elevated(exe: &Path, args: &[String]) -> io::Error {
    // `do shell script` runs with root's environment; keep HOME so config
    // lookups (e.g. the saved theme) still find the invoking user's files.
    let home = std::env::var("HOME").unwrap_or_default();
    let script = format!(
        "do shell script {} with administrator privileges",
        applescript_quote(&elevated_shell_command(&home, exe, args))
    );
    match std::process::Command::new(OSASCRIPT)
        .arg("-e")
        .arg(script)
        .output()
    {
        Ok(out) if out.status.success() => std::process::exit(0),
        Ok(out) => io::Error::new(
            io::ErrorKind::PermissionDenied,
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ),
        Err(e) => e,
    }
}

/// The shell command the admin prompt runs: start `exe` detached so
/// `do shell script` (which waits for its command) returns right away.
fn elevated_shell_command(home: &str, exe: &Path, args: &[String]) -> String {
    let mut cmd = format!(
        "HOME={} {}",
        sh_quote(home),
        sh_quote(&exe.to_string_lossy())
    );
    for a in args {
        cmd.push(' ');
        cmd.push_str(&sh_quote(a));
    }
    cmd.push_str(" >/dev/null 2>&1 &");
    cmd
}

/// Single-quote `s` for `sh`.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Double-quote `s` as an AppleScript string literal.
fn applescript_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', r"\\").replace('"', r#"\""#))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environ_of_self_contains_path() {
        let env = environ(std::process::id() as i32).expect("own environment readable");
        assert!(env.iter().any(|kv| kv.starts_with("PATH=")), "{env:?}");
    }

    #[test]
    fn sh_quote_wraps_and_escapes_single_quotes() {
        assert_eq!(sh_quote("plain"), "'plain'");
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
        assert_eq!(
            sh_quote(r#"/Users/x/My "Game"/a b"#),
            r#"'/Users/x/My "Game"/a b'"#
        );
    }

    #[test]
    fn applescript_quote_escapes_quotes_and_backslashes() {
        assert_eq!(applescript_quote(r#"a "b" \c"#), r#""a \"b\" \\c""#);
    }

    #[test]
    fn elevated_command_runs_exe_in_background_with_home() {
        let cmd = elevated_shell_command(
            "/Users/me",
            Path::new("/Apps/cheatu gui"),
            &["--attach".into(), "42".into()],
        );
        assert_eq!(
            cmd,
            "HOME='/Users/me' '/Apps/cheatu gui' '--attach' '42' >/dev/null 2>&1 &"
        );
    }
}
