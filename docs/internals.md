# Internals

## How the engine works

- **First scan** walks the target's readable+writable regions (from
  `/proc/<pid>/maps` on Linux, the Mach VM region list on macOS), reading them
  in 1 MiB chunks and testing each type-aligned slot against your value.
- **Next scan** re-reads only the surviving addresses and applies the chosen
  comparison against either your value or the previously observed value
  (for `increased`/`decreased`/`changed`/`unchanged`).
- Floating-point matches use a small tolerance so a displayed `100` still
  matches an in-memory `99.99998`.

## Scanning performance

- **Multithreaded:** a first scan splits memory into fixed-size work items and
  distributes them across a pool of worker threads (up to 16, sized to your
  CPU) that share one memory handle (`/proc/<pid>/mem` with thread-safe
  `pread` on Linux, the task port on macOS). A ~1 GiB
  process scans in well under a second on a typical multi-core machine.
- **Pause target while scanning** (checkbox, on by default): cheatu sends the
  process `SIGSTOP` for the duration of the scan and `SIGCONT` right after, so
  values can't move mid-scan and there's no CPU contention. Turn it off if you
  need the game to keep running during the scan.

## Workspace layout

| Crate | Purpose |
|-------|---------|
| `cheatu-core` | The engine: process listing, memory maps, memory read/write, typed scan values, the first/next scan algorithm, address expressions (`addr_expr`), cheat table files (`table`), the pointer scanner (`pointer_scan`), AoB signatures (`signature`), and privilege helpers. UI-agnostic; the OS-specific parts live in `src/sys/{linux,macos}.rs`. |
| `cheatu-cli`  | The `cheatu` command-line REPL. |
| `cheatu-gui`  | The `cheatu-gui` graphical app; the cheat wizard is a state machine without egui (`wizard.rs`) plus its window (`wizard_ui.rs`). |
| `cheatu-inject` | The RPG Maker MV/MZ (NW.js) JavaScript-injection backend. |

## Operating-system backends

`cheatu-core/src/sys/` holds one backend per OS behind the same crate-private
interface; the public modules call into it.

| | Linux (`sys/linux.rs`) | macOS (`sys/macos.rs`) |
|---|---|---|
| Read / write memory | `/proc/<pid>/mem` (`pread`/`pwrite`) | `task_for_pid` → `mach_vm_read_overwrite` / `mach_vm_write` (read-only pages are made writable just for the write) |
| Memory map | parse `/proc/<pid>/maps` | `mach_vm_region_recurse` + `proc_regionfilename`; malloc/stack tags become `[heap]`/`[stack]` |
| Process list | walk `/proc` | `proc_listallpids` + `KERN_PROCARGS2` + `proc_pidinfo` |
| Pause during scan | `SIGSTOP` / `SIGCONT` | same |
| Memory budget | `MemAvailable` from `/proc/meminfo` | free + inactive pages (`host_statistics64`) |
| Elevation | `pkexec` | `osascript … with administrator privileges` (GUI) |

## Testing

```sh
cargo test        # or: make test
```

The engine tests scan and patch the test process's *own* memory (always
accessible: `/proc/self/mem` on Linux, the process's own task port on macOS),
so they validate the full read → scan → narrow → write pipeline without
needing root. `crates/cheatu-core/tests/macos.rs` adds macOS-only backend
tests.
