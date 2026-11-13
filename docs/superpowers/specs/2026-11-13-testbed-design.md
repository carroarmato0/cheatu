# cheatu-testbed: a target program for testing cheatu

Date: 13/11/2026 · Branch: `feat/testbed` (on top of `feat/cheat-tables`)

## Goal

A small program with known memory features that cheatu can attach to, used
two ways from one binary:

- **Interactive:** a line-based mini dungeon game you play in a terminal
  while trying cheatu-gui (wizard, tables, pointer scanner) by hand, without
  risking a real game.
- **Scripted (`--script`):** the same game driven over stdin/stdout by
  automated tests, which compare cheatu's results against the true addresses
  the program reports.

Because the game and the test target are the same code, what you try by hand
is exactly what the tests check.

Why now: the core tests scan cheatu's own test process, so AoB and signature
tests had to use simulated memory (a self-scan sees the test's own copies of
the search bytes), and nothing tests a real restart (new pid, ASLR, different
heap layout). A separate process fixes both.

## Non-goals

- No GUI tests; the wizard is tested by hand against the testbed.
- Not packaged or installed; `BINS` in the Makefile and `packaging/` stay
  unchanged.
- No timers or randomness in game logic; values change only through
  commands.
- Replacing the slow macOS pointer tests (~2.5 min) is a possible follow-up,
  not part of this work.

## Crate

- `crates/cheatu-testbed`, `publish = false`, one binary `cheatu-testbed`,
  added to the workspace `members`.
- No runtime dependencies (std only). `cheatu-core` is a dev-dependency for
  the integration tests.
- No `unsafe` beyond what the scenarios strictly need; statics use atomics
  (`AtomicU32`, `AtomicU64`, `AtomicPtr`).
- `make testbed` builds and runs it in interactive mode.

## Memory scenarios

| Name | Type | Location | What it exercises |
|---|---|---|---|
| `gold` | u32 | static, non-zero initial value (`.data`) | `"module"+0x…` expressions |
| `kills` | u64 | static, zero initial value (`.bss`) | the Linux `.bss` tail of the module |
| `gold_shown` | u32 | static, copied from `gold` after every command | decoy: two addresses change in lockstep; writing it has no lasting effect |
| `hp` | i32 | heap: `static WORLD → Level → Player` | pointer chain, 3 dereferences from a static root |
| `mana` | f32 | `Player` | float scans |
| `xp` | f64 | `Player` | double scans |
| `ammo` | i16 | heap `Inventory` (owned by `Player`), between fixed byte fields | the signature helper on a real process |
| `marker` | 16 bytes | first field of `Player` | `aob("…")` expressions |

- `Level`, `Player` and `Inventory` are `#[repr(C)]` with fixed field order so
  offsets are stable across builds of the same source.
- **Moving values:** `next` drops `Level`, `Player` and `Inventory` and
  allocates new ones, with a few dummy allocations kept alive in between, so
  every heap address changes while the chain from `WORLD` stays valid.
  Values carry over to the new level.
- **Marker:** built at runtime from a constant transformed at startup (e.g.
  XOR), so the plain 16 bytes exist only inside `Player`, not in the binary's
  read-only data.
- **Inventory bytes around `ammo`:** fixed, distinctive values that never
  change, so `make_signature` can find a unique window.

## Game

Starting values: gold 100, kills 0, hp 100 (max 100), mana 50.0, xp 0.0,
ammo 10.

| Command | Effect |
|---|---|
| `status` | print all values (reading them from memory each time) |
| `fight` | hp −7, xp +12.5, kills +1, gold +15 |
| `heal` | mana −10.0, hp +20 (capped at max); refused below 10.0 mana |
| `shoot` | ammo −1; refused at 0 |
| `buy` | gold −20, ammo +5; refused below 20 gold |
| `next` | new level (reallocate, see above), mana +25.0 |
| `help` | list commands |
| `quit` | exit |

- When hp reaches 0 or below the game prints "you died" and resets every
  value to its starting value (allocations included). This gives a
  "value went back down" case for next scans.
- After every command, `gold_shown` is set to `gold`.
- Interactive mode prints a banner with the pid (so you can find it in
  cheatu's process list) and a status line after every command.

## Scripted mode (`--script`)

- First line on stdout: `READY pid=<pid>`.
- Every input line yields exactly one output line, beginning with `OK` or
  `ERR <reason>`. No prompts, no banner, stdout flushed after each line.
- Game commands behave as above (`OK` plus the new values, or `ERR` when
  refused).
- Extra commands:
  - `addrs` → `OK gold=0x… kills=0x… gold_shown=0x… hp=0x… mana=0x… xp=0x… ammo=0x… marker=0x… world=0x…`
    (hex, true current addresses).
  - `get <name>` → `OK <value>` as the game reads it (decimal; floats in
    Rust's default `Display`).
  - `marker` → `OK <32 hex digits>` (the marker bytes, for building the
    `aob()` pattern).
- EOF on stdin exits the program.

## Integration tests (`crates/cheatu-testbed/tests/`)

A `Testbed` helper spawns `env!("CARGO_BIN_EXE_cheatu-testbed") --script`,
reads `READY`, offers `cmd(&str) -> String` and `addr(name) -> u64`, and kills
the child on drop.

Tests (each compares against `addrs` / `get`):

1. **Scan:** exact u32 scan for gold, `fight`, next scan for the new value →
   results contain both `gold` and `gold_shown`.
2. **Static expressions:** build `"cheatu-testbed"+0x…` for `gold` and `kills`
   from module base; after a restart they resolve to the new true addresses.
3. **Pointer chains:** pointer scan from `hp` finds ≥1 chain rooted in the
   testbed module; after a restart, filtering by the new `hp` address leaves
   ≥1 chain, every survivor resolves to it, and they still resolve to `hp`
   after `next`.
4. **AoB:** `aob("<marker>")` resolves to the marker after a restart;
   `make_signature` on `ammo` yields an expression that resolves to the new
   `ammo` after a restart and after `next`.
5. **Table round trip:** a table with static, pointer and AoB entries is saved,
   the testbed restarted, the table loaded, every entry resolved, and a value
   written through each; `get` shows the written values (gold through its
   static entry, hp through the chain, ammo through the signature).

The executable name is platform-specific (`cheatu-testbed` everywhere, no
extension); tests take the module name from the spawned path's file name.

## Permissions

- On Linux a parent may read its child's memory under the default Yama
  `ptrace_scope=1`, so tests run without root.
- On macOS reading another process requires root unless something else
  allows it. If attaching fails with `PermissionDenied`, a test prints
  `skipped: no permission to attach` and passes. With
  `CHEATU_TESTBED_REQUIRE=1` it fails instead (for Linux and CI).
- **Measurement first (Task 1):**
  - Does ad-hoc signing the testbed with the `com.apple.security.get-task-allow`
    entitlement let a non-root test process attach on macOS? If yes, the build
    signs it that way (a small build step) and macOS tests run without sudo.
  - Do the tests attach inside the podman Linux container used for CI-style
    runs? If it needs `--cap-add SYS_PTRACE` or similar, document it.
  - Record results in a "Measurements" section of this spec.

## Documentation

- `docs/testbed.md`: what the testbed is; how to run it (`make testbed`);
  the scenario table; step-by-step exercises with cheatu-gui: find gold
  (and notice the decoy), freeze hp, make a table and survive a restart,
  pointer-scan hp and survive `next`, use the wizard.
- A line in `docs/internals.md` (testing section) and the README pointing to
  it.

## Risks

- The heap layout can make a chain's offsets differ between platforms; tests
  assert on behaviour (resolves to the true address), never on specific
  offsets.
- `make_signature` samples twice 50 ms apart; the testbed is idle between
  commands, so samples are stable.
- If Task 1 shows macOS needs root, macOS tests only run under
  `sudo -E cargo test -p cheatu-testbed`; Linux remains the authoritative run.

## Measurements

13/11/2026, macOS (Darwin 27.2, Apple Silicon), uid 501, no sudo. A throwaway
probe spawned a small Rust child and called `LiveSource::open(child_pid)`.

| Case | Result |
|---|---|
| macOS, child ad-hoc signed by the linker (no entitlements) | `PermissionDenied`: "permission denied for process …: run cheatu as root (elevate) to scan it" |
| macOS, child re-signed `codesign --force --sign - --entitlements` with `com.apple.security.get-task-allow` | `OK` (191 regions), as a normal user |
| Linux, podman `rust:1`, default (root in container) | `OK` (24 regions) |
| Linux, podman `rust:1`, `--user 1000:1000`, no extra caps (`ptrace_scope` = 0 there) | `OK` (24 regions) |

Rulings:

- The test harness signs the testbed with
  `crates/cheatu-testbed/entitlements.plist` (get-task-allow) once per test
  process before the first spawn, so macOS tests run without sudo.
  `make testbed` signs it the same way, so a non-root cheatu-gui can attach
  to it for manual play.
- No extra container flags are needed. Hosts with Yama `ptrace_scope=1` still
  allow it (the test is the child's parent); `ptrace_scope` ≥ 2 does not, and
  tests then skip unless `CHEATU_TESTBED_REQUIRE=1`.
