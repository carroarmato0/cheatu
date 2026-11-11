# macOS support (phase 1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** cheatu's core, CLI and egui GUI build, test and work on macOS (Apple Silicon) without changing Linux behaviour.

**Architecture:** A private `cheatu-core::sys` module holds one backend per OS (`linux.rs` = today's `/proc` code moved verbatim; `macos.rs` = Mach VM + libproc + sysctl). The public modules `mem`, `maps`, `process`, `privilege` and `scan::candidate_cap` keep their signatures and delegate to `sys`. GUI/CLI changes are limited to config path, CJK fonts and elevation wording.

**Tech Stack:** Rust (stable 1.99, edition 2021), `libc 0.2`, new `mach2` crate (macOS-only dependency), egui/eframe 0.29.

**Spec:** `docs/superpowers/specs/2026-11-11-macos-support-phase1-design.md`

## Global Constraints

- Linux behaviour must be identical: Linux code is **moved, not rewritten**; Linux user-facing strings stay byte-for-byte the same.
- Public API of `cheatu-core` keeps current signatures, except `privilege::pkexec_available()` → `privilege::elevation_available()` plus new `privilege::elevation_tool() -> &'static str` (`"pkexec"` on Linux, `"the macOS admin prompt"` on macOS).
- Unsupported OS: `compile_error!("cheatu supports Linux and macOS only")`.
- `mach2` is a `[target.'cfg(target_os = "macos")'.dependencies]` entry in `cheatu-core` only.
- `RUSTFLAGS=-D warnings`, `cargo fmt --all --check` and `cargo clippy --workspace --all-targets --locked` must stay clean on both OSes.
- Every commit message ends with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Cargo binaries live in `$(brew --prefix rustup)/bin`; prefix commands with `export PATH="$(brew --prefix rustup)/bin:$PATH";` if `cargo` is not found.

### Linux verification command (used by several tasks)

`LINUX_CHECK` =
```sh
rustup target add x86_64-unknown-linux-gnu   # once
cargo clippy --workspace --all-targets --target x86_64-unknown-linux-gnu
podman machine start 2>/dev/null; podman run --rm -v "$PWD":/work -w /work docker.io/library/rust:1 \
  sh -c 'apt-get update -qq && apt-get install -y -qq libxkbcommon-dev libwayland-dev libx11-dev libxcursor-dev libxrandr-dev libxi-dev libgl1-mesa-dev >/dev/null && cargo test --workspace --locked'
```
Expected: clippy clean; all tests pass inside the container. If podman cannot run, note it and rely on CI after push.

## Review Focus

1. **Scanning a region that becomes unmapped mid-scan** (target frees memory): `read_at` must return `Err`/short read, never panic — Task 2 test `read_at_unmapped_address_errors`.
2. **Writing into a read-only page** (e.g. a constant in `__DATA_CONST`): write succeeds and original protection is restored — Task 2 test `write_to_read_only_page_restores_protection`.
3. **Attaching to a pid that just exited / does not exist:** `Mem::open` returns an `Err` (not `PermissionDenied` with a hardened-runtime message) — Task 2 test `open_nonexistent_pid_errors`.
4. **Arguments with spaces/quotes during elevation** (game paths like `/Users/x/Games/My "Game"/`): relaunch command must quote safely — Task 4 tests on `sh_quote` / `applescript_quote`.
5. **Huge processes / many regions on macOS** (submap recursion): region walk must terminate and not duplicate regions — Task 2 test `maps_are_sorted_and_non_overlapping`.

---

### Task 1: Split `cheatu-core` into per-OS backends (Linux only, no behaviour change)

**Files:**
- Create: `crates/cheatu-core/src/sys/mod.rs`, `crates/cheatu-core/src/sys/linux.rs`
- Modify: `crates/cheatu-core/src/{lib.rs,mem.rs,maps.rs,process.rs,privilege.rs,scan.rs}`, `crates/cheatu-cli/src/main.rs`, `crates/cheatu-gui/src/main.rs`

**Interfaces (Produces — every backend must provide exactly these, `pub(crate)`):**
```rust
pub(crate) struct MemHandle;                       // Send + Sync
impl MemHandle {
    pub(crate) fn open(pid: i32) -> io::Result<MemHandle>;
    pub(crate) fn read_at(&self, addr: u64, buf: &mut [u8]) -> io::Result<usize>;
    pub(crate) fn write_at(&self, addr: u64, buf: &[u8]) -> io::Result<()>;
}
pub(crate) fn accessible(pid: i32) -> bool;
pub(crate) fn read_maps(pid: i32) -> io::Result<Vec<MemoryRegion>>;
pub(crate) struct RawProc { pub pid: i32, pub name: String, pub cmdline: String, pub rss_bytes: u64 }
pub(crate) fn raw_processes() -> Vec<RawProc>;      // kernel threads / unreadable already skipped
pub(crate) fn environ(pid: i32) -> Option<Vec<String>>; // "KEY=VALUE" entries
pub(crate) fn available_memory() -> Option<usize>;  // bytes
pub(crate) const ELEVATION_TOOL: &str;
pub(crate) fn elevation_available() -> bool;
pub(crate) fn relaunch_elevated(exe: &Path, args: &[String]) -> io::Error; // args already include ELEVATED_FLAG
```
Public changes: `Mem { pid, handle: sys::MemHandle }`; `privilege::elevation_available()`, `privilege::elevation_tool()` replace `pkexec_available()`.

- [ ] **Step 1:** Create `sys/mod.rs` with the cfg-selected `mod linux; pub(crate) use linux::*;` (and the same for `macos`, gated, file created in Task 2) plus the `compile_error!` for other OSes. Add `mod sys;` to `lib.rs`.
- [ ] **Step 2:** Move into `sys/linux.rs` verbatim: the `/proc/<pid>/mem` open/read/write (as `MemHandle`), `accessible`, the `/proc/<pid>/maps` parser body, the `/proc` walk producing `RawProc` (comm, cmdline join, `read_rss`, `page_size`), `/proc/<pid>/environ` reading, `/proc/meminfo` `MemAvailable` parsing, `which("pkexec")`, and the pkexec `Command` + env-forwarding + `exec()`.
- [ ] **Step 3:** Make public modules delegate: `list_processes()` maps `RawProc` → `ProcInfo` using shared `detect_role`, `detect_wine(pid, cmdline)` (keeps `.exe` check, then `sys::environ(pid)` with the same prefix list), and `sys::accessible`. `candidate_cap()` uses `sys::available_memory().unwrap_or(8 << 30)`. `privilege::relaunch_elevated(extra)` keeps the argument-forwarding loop and calls `sys::relaunch_elevated(&exe, &args)`.
- [ ] **Step 4:** Rename callers of `pkexec_available()` in CLI (`main.rs` `elevate`) and GUI (two sites) to `elevation_available()`. Keep every message text unchanged in this task.
- [ ] **Step 5: Verify Linux unchanged.** Run `LINUX_CHECK`. Expected: clippy clean, all existing tests pass. Also `git diff --stat` shows no edits to Linux message strings.
- [ ] **Step 6: Commit** `refactor(core): move Linux /proc and pkexec code behind a sys backend`.

### Task 2: macOS memory access and memory map

**Files:**
- Create: `crates/cheatu-core/src/sys/macos.rs`
- Modify: `crates/cheatu-core/Cargo.toml` (add `[target.'cfg(target_os = "macos")'.dependencies] mach2 = "0.4"`), `Cargo.lock`
- Test: `crates/cheatu-core/tests/macos.rs` (whole file `#![cfg(target_os = "macos")]`), existing `crates/cheatu-core/tests/engine.rs`

**Interfaces:**
- Consumes: Task 1 backend contract.
- Produces: macOS `MemHandle`, `accessible`, `read_maps`, `available_memory`. For this task `raw_processes()` returns `Vec::new()`, `environ()` returns `None`, `elevation_available()` returns `false`, `relaunch_elevated` returns `io::Error::new(Unsupported, ...)` — **all replaced in Tasks 3–4** (leave a `// replaced in task 3/4` note only in this commit).
- Produces (crate-internal, used in Task 4): `fn denied_error(pid: i32, kr: kern_return_t, as_root: bool) -> io::Error`.

- [ ] **Step 1: Write failing tests** in `tests/macos.rs`:
```rust
#[test] fn maps_contain_heap_holding_a_boxed_value() {
    let b = Box::new(0xC0FFEEu32); let addr = &*b as *const u32 as u64;
    let maps = read_maps(own_pid()).unwrap();
    let r = region_for(&maps, addr).expect("boxed value is mapped");
    assert!(r.read && r.write); assert_eq!(r.kind(), RegionKind::Heap);
}
#[test] fn maps_contain_stack_holding_a_local() { /* local u64 → region kind Stack */ }
#[test] fn maps_are_sorted_and_non_overlapping() { /* windows(2): a.end <= b.start */ }
#[test] fn read_at_unmapped_address_errors() { /* Mem::open(self).read_at(0x8, &mut [0;4]) is Err */ }
#[test] fn write_to_read_only_page_restores_protection() {
    static RO: u32 = 0x1234_5678; let addr = &RO as *const u32 as u64;
    let mem = Mem::open(own_pid()).unwrap();
    mem.write_at(addr, &0x8765_4321u32.to_le_bytes()).unwrap();
    let mut back = [0u8; 4]; mem.read_at(addr, &mut back).unwrap();
    assert_eq!(u32::from_le_bytes(back), 0x8765_4321);
    let r = region_for(&read_maps(own_pid()).unwrap(), addr).unwrap().clone();
    assert!(!r.write, "original read-only protection restored");
}
#[test] fn open_nonexistent_pid_errors() { /* Mem::open(i32::MAX) is Err, kind != PermissionDenied */ }
```
- [ ] **Step 2:** Run `cargo test -p cheatu-core`. Expected: compile failure (no macOS backend).
- [ ] **Step 3: Implement `MemHandle`** with `task_for_pid(mach_task_self(), pid, &mut port)`; `Drop` → `mach_port_deallocate`. `read_at` → `mach_vm_read_overwrite`; non-success → `io::Error::other(mach_error_string)`. `write_at` → `mach_vm_write`; on `KERN_PROTECTION_FAILURE`/`KERN_INVALID_ADDRESS` look up the region's `protection` via `mach_vm_region`, `mach_vm_protect(addr page-aligned, len, false, prot | VM_PROT_WRITE | VM_PROT_COPY)`, write, then `mach_vm_protect` back to the original `prot`. `unsafe impl Send/Sync` with a `// SAFETY:` note (task ports are plain integers; Mach VM calls are thread-safe). `open` errors: pid not running (`kill(pid, 0)` fails with `ESRCH`) → `ErrorKind::NotFound`; otherwise `denied_error(pid, kr, geteuid()==0)` → `PermissionDenied`, whose message when `as_root` is exactly: `"macOS refused access to process {pid} even as root — it is most likely protected by the hardened runtime or System Integrity Protection. Re-sign the game ad-hoc without the hardened runtime (codesign --force --sign - <app>) or disable SIP to scan it."`; when not root: `"permission denied for process {pid}: run cheatu as root (elevate) to scan it"`.
- [ ] **Step 4: Implement `read_maps`** — loop `mach_vm_region_recurse` with `VM_REGION_SUBMAP_INFO_COUNT_64`, starting at address 0 and `depth = 0`; when `is_submap != 0`, `depth += 1` and retry the same address; else push the region, advance `addr += size`. Stop on `KERN_INVALID_ADDRESS`. Map `protection` → read/write/exec, `share_mode ∈ {SM_SHARED, SM_TRUESHARED, SM_SHARED_ALIASED}` → `shared`. `path`: `proc_regionfilename(pid, addr, buf)` if > 0; else `user_tag` `VM_MEMORY_MALLOC..=VM_MEMORY_MALLOC_NANO` (1–11, plus 53 `VM_MEMORY_MALLOC_MEDIUM` if exposed by `mach2`, else define the constants locally) → `"[heap]"`, `VM_MEMORY_STACK` (30) → `"[stack]"`, else `""`.
- [ ] **Step 5: Implement `accessible`** (open + drop) and **`available_memory`** via `host_statistics64(mach_host_self(), HOST_VM_INFO64)` → `(free_count + inactive_count) * page_size`, falling back to `sysctlbyname("hw.memsize") / 2`.
- [ ] **Step 6:** Run `cargo test -p cheatu-core`. Expected: all of `tests/macos.rs`, `tests/engine.rs` and `scan.rs` unit tests (incl. `candidate_cap_is_sane`) PASS.
- [ ] **Step 7:** `cargo clippy -p cheatu-core --all-targets` clean; `LINUX_CHECK` clippy part clean.
- [ ] **Step 8: Commit** `feat(core): macOS memory read/write and region map via Mach VM`.

### Task 3: macOS process listing

**Files:**
- Modify: `crates/cheatu-core/src/sys/macos.rs`
- Test: `crates/cheatu-core/tests/macos.rs`

**Interfaces:**
- Consumes: `RawProc`, `environ` contract (Task 1).
- Produces: real `raw_processes()` and `environ()` on macOS.

- [ ] **Step 1: Write failing tests:**
```rust
#[test] fn process_list_contains_self() {
    let me = list_processes().into_iter().find(|p| p.pid == own_pid()).expect("self listed");
    assert!(!me.name.is_empty()); assert!(me.cmdline.contains("macos")); // test binary name
    assert!(me.rss_bytes > 0); assert!(me.accessible);
}
#[test] fn process_list_is_sorted_by_pid() { /* windows(2): a.pid < b.pid */ }
```
And in `sys/macos.rs` `#[cfg(test)] mod tests` (environ is crate-private):
```rust
#[test] fn environ_of_self_contains_path() {
    let env = environ(std::process::id() as i32).expect("own environment readable");
    assert!(env.iter().any(|kv| kv.starts_with("PATH=")));
}
```
- [ ] **Step 2:** `cargo test -p cheatu-core --test macos`. Expected: FAIL (`self listed`).
- [ ] **Step 3: Implement.** `libc::proc_listallpids` (size query with null buffer, then fill; skip pid 0). Per pid: `proc_name` → `name`; `sysctl([CTL_KERN, KERN_PROCARGS2, pid])` → parse layout `argc: i32`, exec path (NUL-terminated, then NUL padding), `argc` NUL-terminated argv strings, then env strings until an empty string; `cmdline = argv.join(" ")`. If sysctl fails: `cmdline = proc_pidpath` result; if that is empty too, skip the pid. `rss_bytes` = `proc_pidinfo(PROC_PIDTASKINFO)` `pti_resident_size` or 0. `environ(pid)` reuses the same KERN_PROCARGS2 parser (one private `fn procargs(pid) -> Option<(Vec<String>, Vec<String>)>`).
- [ ] **Step 4:** `cargo test -p cheatu-core`. Expected: PASS.
- [ ] **Step 5:** Run `cargo run -p cheatu-cli` and type `list` (or the CLI's process-list command per `help`). Expected: real processes with sizes; own shell processes marked accessible.
- [ ] **Step 6: Commit** `feat(core): list macOS processes via libproc and KERN_PROCARGS2`.

### Task 4: macOS elevation

**Files:**
- Modify: `crates/cheatu-core/src/sys/macos.rs`
- Test: `#[cfg(test)] mod tests` in `sys/macos.rs`

**Interfaces:**
- Produces: `ELEVATION_TOOL = "the macOS admin prompt"`, `elevation_available() -> true` (when `/usr/bin/osascript` is a file), `relaunch_elevated(exe, args)`, private `fn sh_quote(s: &str) -> String`, `fn applescript_quote(s: &str) -> String`.

- [ ] **Step 1: Write failing tests:**
```rust
assert_eq!(sh_quote("plain"), "'plain'");
assert_eq!(sh_quote("it's"), r"'it'\''s'");
assert_eq!(sh_quote(r#"/Users/x/My "Game"/a b"#), r#"'/Users/x/My "Game"/a b'"#);
assert_eq!(applescript_quote(r#"a "b" \c"#), r#""a \"b\" \\c""#);
```
- [ ] **Step 2:** `cargo test -p cheatu-core sh_quote`. Expected: FAIL (not defined).
- [ ] **Step 3: Implement.** Shell command = `HOME=<sh_quote(home)> <sh_quote(exe)> <sh_quote(arg)>… >/dev/null 2>&1 &` (home from `$HOME`). Run `/usr/bin/osascript -e "do shell script {applescript_quote(cmd)} with administrator privileges"` with `.output()`. Success → `std::process::exit(0)`. Failure → `io::Error::new(PermissionDenied, stderr trimmed)` (user cancel shows osascript's "User canceled." text).
- [ ] **Step 4:** `cargo test -p cheatu-core`. Expected: PASS.
- [ ] **Step 5: Manual:** `cargo run -p cheatu-cli`, run `sudo` command, authenticate. Expected: a new elevated `cheatu` is started — **note**: a backgrounded CLI loses its terminal, so for the CLI print instead `"on macOS, restart cheatu with: sudo cheatu"` and skip relaunch (CLI only; see Task 5). Verify the relaunch path through the GUI in Task 5.
- [ ] **Step 6: Commit** `feat(core): elevate on macOS through the native admin prompt`.

### Task 5: GUI and CLI adjustments

**Files:**
- Modify: `crates/cheatu-gui/src/main.rs` (`config_path` ~L239, `cjk_font_path` ~L275, elevation sites ~L1827 and ~L2753, existing `#[cfg(test)]` module ~L2907), `crates/cheatu-cli/src/main.rs` (banner ~L28, `elevate` ~L136, help text ~L475)

- [ ] **Step 1: Write failing test** in the GUI's test module:
```rust
#[cfg(target_os = "macos")]
#[test] fn config_lives_in_application_support_on_macos() {
    assert!(config_path().ends_with("Library/Application Support/cheatu/config.json"));
}
```
- [ ] **Step 2:** `cargo test -p cheatu-gui`. Expected: FAIL.
- [ ] **Step 3: Implement.**
  - `config_path()`: `#[cfg(target_os = "macos")]` → `home_dir().join("Library/Application Support/cheatu/config.json")`; Linux body unchanged.
  - `cjk_font_path()`: append `"/System/Library/Fonts/Hiragino Sans GB.ttc"` and `"/System/Library/Fonts/AppleSDGothicNeo.ttc"` to the fallback list.
  - GUI messages: `"Relaunch with root via pkexec and re-attach to this process"` → `format!("Relaunch with root via {} and re-attach to this process", privilege::elevation_tool())`; `"Requesting elevation via pkexec…"` → `format!("Requesting elevation via {}…", privilege::elevation_tool())`. On Linux these render identically.
  - CLI banner: `"cheatu {} — Linux memory scanner"` → OS word from `cfg!` (`"Linux"` / `"macOS"`). Note line: Linux unchanged; macOS: `"note: not running as root; reading game memory usually needs it.\nrestart with `sudo cheatu` to scan other processes."`.
  - CLI `elevate()` on macOS: print `"on macOS, restart cheatu with: sudo cheatu"` and return (see Task 4 Step 5). Linux path unchanged. Help line for `sudo`: macOS shows `"sudo                 explain how to run with root privileges"`.
- [ ] **Step 4:** `cargo test --workspace` and `cargo clippy --workspace --all-targets`. Expected: PASS / clean. `LINUX_CHECK` clippy clean.
- [ ] **Step 5: Manual (GUI):** write a tiny target in the scratchpad (`loop { println!("{}", v); sleep(2s); v += 1 }` with `v: i32` starting at 1000, built with `rustc`), run it, then `cargo run --release -p cheatu-gui`: pick the target in the process picker, first-scan its value, wait, next-scan "increased", write 9999, confirm the target prints 9999; freeze and confirm it stays. Then click "Request root access", authenticate, and confirm an elevated GUI opens and attaches when started via "Elevate & attach" on a root-owned or other process. Check Japanese text renders (RPG Maker tab labels or a paste of `日本語` into a text field).
- [ ] **Step 6: Commit** `feat(gui,cli): macOS config path, CJK fonts and elevation wording`.

### Task 6: CI and documentation

**Files:**
- Modify: `.github/workflows/ci.yml`, `README.md`, `docs/internals.md`, `docs/packaging.md`

- [ ] **Step 1:** Add a `macos` job to `ci.yml`: `runs-on: macos-latest`, steps checkout → `dtolnay/rust-toolchain@stable` (clippy) → `Swatinem/rust-cache@v2` → `cargo clippy --workspace --all-targets --locked` → `cargo test --workspace --locked`. Leave the existing jobs untouched.
- [ ] **Step 2:** README: badge `platform-Linux%20%7C%20macOS`; tagline "for Linux and macOS"; add a "🍎 macOS" section: build (`brew install rustup && rustup default stable && cargo build --release`), run as root (`sudo ./target/release/cheatu` or the GUI's "Request root access"), hardened runtime / SIP caveat with the same remedy text as Task 2's message, and Wine/CrossOver/Whisky games work. `docs/internals.md`: add the per-OS backend table from the spec and replace the Linux-only test note ("own memory via `/proc/self/mem` on Linux, `task_for_pid` on itself on macOS"). `docs/packaging.md`: short "Building on macOS" section.
- [ ] **Step 3:** `cargo fmt --all --check`; validate YAML with `python3 -c "import yaml,sys;yaml.safe_load(open('.github/workflows/ci.yml'))"`. Expected: no output / no error.
- [ ] **Step 4: Commit** `ci,docs: macOS build job and documentation`.
- [ ] **Step 5: Final verification:** `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked && cargo test --workspace --locked` on macOS, plus full `LINUX_CHECK`. Expected: all green. Do not push without the user's go-ahead.
