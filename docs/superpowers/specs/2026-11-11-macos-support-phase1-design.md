# macOS support — Phase 1: core, CLI and egui GUI

Date: 11/11/2026
Status: approved design, pending spec review

## Context

cheatu is Linux-only: `cheatu-core` talks to `/proc/<pid>/{maps,mem,cmdline,statm,environ}`,
`/proc/meminfo`, and elevates through `pkexec`. The scan engine (`scan.rs`,
`value.rs`) is already OS-agnostic — it only consumes `MemoryRegion`s and
`Mem::read_at`/`write_at`. The CLI, the egui GUI and `cheatu-inject` (CDP over
WebSocket) have only small Linux couplings (pkexec wording, `~/.config`, CJK
font paths).

macOS support ships in two phases:

1. **Phase 1 (this spec):** macOS backend in `cheatu-core`; CLI and the existing
   egui GUI build and work on macOS.
2. **Phase 2 (separate spec):** a native SwiftUI `Cheatu.app` on top of the Rust
   core via a UniFFI `cheatu-ffi` crate. Linux keeps the egui GUI.

## Goals

- `cargo build --workspace`, `cargo clippy --workspace --all-targets` and
  `cargo test --workspace` pass on macOS (Apple Silicon) and Linux.
- On macOS, the CLI and egui GUI can list processes, attach to an accessible
  process, first/next scan, write, freeze and probe.
- Linux behaviour, CLI output, packaging and CI jobs are unchanged.

## Non-goals

- SwiftUI front-end, `.app` bundle, signing, notarization (phase 2 or later).
- Debugger-entitlement (`com.apple.security.cs.debugger`) route to avoid root —
  documented as a future option only.
- Testing on Intel Macs (should compile; not verified).
- Live-testing `cheatu-inject` against an RPG Maker game on macOS (it must
  compile; its CDP transport is OS-agnostic).
- Windows or other OSes. Unsupported targets fail at compile time with a clear
  `compile_error!`.

## Design

### 1. OS backends in `cheatu-core`

New private module `cheatu-core/src/sys/` with `linux.rs` and `macos.rs`,
selected by `#[cfg(target_os = "...")]`. Each backend provides the same set of
functions/types; the public modules keep their **current signatures** and
delegate to `sys`:

| Public API | Linux backend | macOS backend |
|---|---|---|
| `Mem::open/read_at/write_at` | `/proc/<pid>/mem` + `pread`/`pwrite` (moved as-is) | `task_for_pid` → `mach_vm_read_overwrite` / `mach_vm_write` |
| `mem::accessible(pid)` | open `/proc/<pid>/mem` | `task_for_pid` succeeds (port released immediately) |
| `read_maps(pid)` | parse `/proc/<pid>/maps` (moved as-is) | walk `mach_vm_region_recurse` |
| `list_processes()` | `/proc` walk (moved as-is) | `proc_listallpids` + `proc_name` + `KERN_PROCARGS2` + `proc_pidinfo` |
| `suspend/resume` | `SIGSTOP`/`SIGCONT` | same (POSIX; shared code) |
| `scan::candidate_cap()` | `/proc/meminfo` `MemAvailable` | `host_statistics64` free+inactive pages, fallback `hw.memsize / 2` |
| `privilege::*` | pkexec | osascript admin prompt |

Shared, OS-independent logic stays in the public modules (e.g. `MemoryRegion`,
`RegionKind`, `detect_role`, `human_bytes`, `ProcInfo::label`, `probe_address`).
`detect_wine` keeps the `.exe` + environment-variable heuristic; each backend
only supplies the raw cmdline/environment.

The Linux code is moved, not rewritten: Linux behaviour must be byte-for-byte
the same.

#### macOS `Mem`

- Holds the `mach_port_t` task port; `Drop` calls `mach_port_deallocate`.
  `Mem` remains `Send + Sync` (the scanner shares one handle across worker
  threads, as it does with the `/proc` file on Linux; Mach VM calls on a task
  port are thread-safe).
- `read_at` uses `mach_vm_read_overwrite`. On `KERN_SUCCESS` returns the
  transferred size; on failure returns an `io::Error` (callers already treat
  errors/short reads as "skip region").
- `write_at` tries `mach_vm_write`. On `KERN_PROTECTION_FAILURE` it queries the
  region's current protection, `mach_vm_protect`s it to add `VM_PROT_WRITE`
  (with `VM_PROT_COPY` for copy-on-write mappings), writes, and restores the
  original protection.
- `kern_return_t` failures map to `io::Error`: `KERN_FAILURE`/`KERN_PROTECTION_FAILURE`
  from `task_for_pid` → `ErrorKind::PermissionDenied` (so the existing GUI/CLI
  "needs elevation" paths trigger), everything else → `ErrorKind::Other` with
  the Mach error string (`mach_error_string`).

#### macOS memory map

- Iterate with `mach_vm_region_recurse` (`VM_REGION_SUBMAP_INFO_64`),
  descending into submaps, producing `MemoryRegion { start, end, read, write,
  exec, shared, path }` from `protection` and `share_mode`.
- `path`: `proc_regionfilename` for file-backed regions. For anonymous regions,
  synthesize Linux-style pseudo-names from `user_tag` so `RegionKind`
  classification and address hints work unchanged:
  - `VM_MEMORY_MALLOC*` tags → `"[heap]"`
  - `VM_MEMORY_STACK` → `"[stack]"`
  - otherwise `""` (anonymous).
- `is_scannable` keeps its rules; on macOS there are no `[vvar]`/`[vdso]`
  regions, so only the read+write check matters. Regions tagged as shared
  libraries' read-only text are skipped naturally by the write check.

#### macOS process list

- `proc_listallpids` → pids; `proc_name` → short name.
- `sysctl({CTL_KERN, KERN_PROCARGS2, pid})` → argc, exec path, argv, env.
  argv joined with spaces = `cmdline`; env feeds `detect_wine` (CrossOver,
  Whisky, Wine set `WINEPREFIX` etc.). Processes whose args are unreadable
  (other users, kernel/system without privilege) fall back to `proc_pidpath`
  for `cmdline`; processes with neither are skipped (mirrors skipping kernel
  threads on Linux).
- `rss_bytes` from `proc_pidinfo(PROC_PIDTASKINFO).pti_resident_size`,
  best-effort 0.
- `accessible` = `mem::accessible(pid)`.

Note: calling `task_for_pid` for every process during listing is the macOS
analogue of Linux's open-test and is cheap; if it proves slow in practice the
list can switch to "accessible = we are root or pid is self" and verify on
attach.

### 2. Privilege elevation

- `is_root()` unchanged (`geteuid() == 0`).
- `pkexec_available()` is replaced by `elevation_available()` plus
  `elevation_tool() -> &'static str` (`"pkexec"` / `"the macOS admin prompt"`)
  for user-facing messages. Callers in the CLI and GUI are updated; their
  Linux wording stays identical.
- macOS `relaunch_elevated(extra_args)`:
  - Builds a shell command: `HOME=<home> '<current_exe>' <forwarded args…>
    <extra_args…> --already-elevated >/dev/null 2>&1 &`, every argument
    single-quote-escaped.
  - Runs `osascript -e 'do shell script "<cmd>" with administrator privileges'`
    (AppleScript string escaping applied) and waits for it.
  - If osascript succeeds (authenticated; the elevated instance is now running
    in the background) the current process exits with status 0. If the user
    cancels or authentication fails, returns an `io::Error` carrying osascript's
    message, the same contract as the pkexec path today ("only returns on
    failure").
  - `--attach <pid>` and `--already-elevated` handling is shared and unchanged.
- Hardened-runtime explanation: when running as root and `task_for_pid` still
  fails, the backend returns a `PermissionDenied` error whose message explains
  that the target is likely protected by the hardened runtime / SIP and lists
  the options (re-sign the game ad-hoc without the hardened runtime, or disable
  SIP). The GUI and CLI already display error messages; no new UI.

### 3. GUI and CLI adjustments

- Config path: on macOS `~/Library/Application Support/cheatu/config.json`;
  Linux keeps `$XDG_CONFIG_HOME/cheatu` / `~/.config/cheatu`.
- `cjk_font_path()` appends macOS system fonts to its fallback list (verified
  present on this Mac): `/System/Library/Fonts/Hiragino Sans GB.ttc` (CJK
  ideographs + kana) and `/System/Library/Fonts/AppleSDGothicNeo.ttc` (Hangul);
  first existing wins. The `CHEATU_CJK_FONT` override and `fc-match` step stay
  (fc-match is simply absent on a stock Mac).
- Messages that hard-code "pkexec" use `privilege::elevation_tool()`.
- No other egui changes; macOS visual polish belongs to phase 2.

### 4. Testing

- Existing `cheatu-core/tests/engine.rs` scans the test process's own memory.
  On macOS `task_for_pid(mach_task_self())` works without root, so the same
  tests run on both OSes. Any test that reads `/proc` directly gets a
  platform-neutral replacement or a `#[cfg(target_os = "linux")]` gate.
- New macOS-only unit tests: `read_maps(self)` contains a heap region and the
  region holding a local stack variable; `list_processes()` contains our own
  pid with a non-empty name; `Mem` write to a read-only page of our own process
  succeeds via the protect/restore path; `candidate_cap()` is sane.
- Manual verification on this Mac: run `cheatu` and `cheatu-gui` against a
  small non-hardened test program (scan → narrow → write → freeze), and with
  `sudo` against another process.

### 5. CI and docs

- `ci.yml`: add a `macos` job on `macos-latest` running clippy and tests
  (fmt is OS-independent and stays in the Linux job). Linux jobs untouched.
- README: platform badge "Linux | macOS", a macOS install/build section, and
  the macOS caveats (root/admin prompt, hardened runtime/SIP).
- `docs/internals.md`: per-OS backend table; `docs/packaging.md`: building on
  macOS (`brew install rustup`, `cargo build --release`).

## Risks

- **Hardened runtime / SIP** blocks many notarized games even as root. Mitigated
  only by clear messaging in phase 1.
- **GUI as root on macOS** (via the osascript relaunch) works but is not
  Apple's preferred model; phase 2 can revisit with a privileged helper or the
  debugger entitlement.
- **Rosetta (x86_64) targets:** `task_for_pid` and VM reads work on translated
  processes; data layout is the game's own, so scans behave as on Intel. Not
  separately tested.
