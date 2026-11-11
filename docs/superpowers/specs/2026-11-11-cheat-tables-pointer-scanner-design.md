# Cheat tables, pointer scanner and cheat wizard

Date: 11/11/2026
Status: approved design, pending spec review

## Context

cheatu's cheat table (`SavedEntry` in `cheatu-gui/src/main.rs`) holds a fixed
address, type, value text and freeze flag. It lives only in memory: it cannot
be saved, and a fixed address is useless after the game restarts because heap
and ASLR addresses move.

A real session on macOS (The Legend of Zelda: Ocarina of Time running in a
recompilation runtime, `dmhost`) showed the gap: finding the rupee counter took
three value scans, a manual test of each candidate, and the result lived in
anonymous memory, so neither the address nor "module + offset" survives a
restart.

This work is split in two specs:

1. **This spec:** saveable cheat tables whose entries locate their address with
   an *address expression* (module + offset, pointer chains, AoB patterns); a
   pointer scanner that finds pointer chains to an address and narrows them
   across restarts; a wizard that guides less experienced users through the
   whole flow.
2. **Later spec:** sandboxed scripting for what an expression cannot express.
   Cheat tables are designed so scripts can be added without breaking files.

## Goals

- Save and open cheat tables as a readable, shareable TOML file, from the GUI
  and the CLI.
- Every entry's address is an expression that is re-resolved against the
  running process, so a table keeps working across game restarts when its
  expressions are stable.
- A pointer scanner that finds pointer chains from static (module) memory to an
  address, saves them, and filters them after a restart until robust chains
  remain.
- A GUI wizard: find a value, find out which candidate is the real one, make it
  survive restarts, save it as a named cheat. No Cheat Engine knowledge needed.
- Loading a table, including one downloaded from someone else, never writes to
  the target's memory by itself.
- Linux and macOS, same as the rest of cheatu.

## Non-goals

- Scripting of any kind (later spec).
- Importing or exporting Cheat Engine `.CT` files.
- Support aimed at a specific emulator or runtime (cheat formats of
  Ryujinx/yuzu/Atmosphère, guest address translation).
- Pointer scanner extras: comparing several pointer maps in one go, thread
  stacks as base ("threadstack" in Cheat Engine), 32-bit targets.
- A wizard in the CLI. The CLI gets the underlying commands only.

## Design

### 1. Address expressions (`cheatu-core::addr_expr`)

One line that names an address the same way in the GUI, the CLI and the table
file.

```
"dmhost"+0x1a2b30                               module + offset
[["dmhost"+0x1a2b30]+0x48]+0x6337e134           pointer chain
aob("48 8B 05 ?? ?? ?? ?? 89", "dmhost")+0x10   byte pattern, limited to a module
0x7ccd2e779c                                    plain address (does not survive restarts)
```

Grammar:

```
expr    := term (('+' | '-') term)*
term    := number | module | deref | aob | '(' expr ')'
number  := '0x' hexdigits | decdigits
module  := '"' name '"'
deref   := '[' expr ']'
aob     := 'aob(' '"' pattern '"' (',' '"' name '"')? ')'
```

Whitespace is allowed between tokens. Hex needs the `0x` prefix, so `10` is
always ten. The pattern syntax is the existing `parse_aob` syntax (`??`
wildcards).

Semantics:

- `"name"`: the base of the loaded image whose file name (the last path
  component of the region's path) equals `name`, exactly and case-sensitively.
  The base is the lowest start address of that image's regions. No match is an
  error; two different images with the same file name is an error naming both
  paths.
- `[x]`: read 8 bytes at `x`, little-endian. cheatu only supports 64-bit
  targets.
- `aob(p)` / `aob(p, "name")`: search readable regions (of that module only,
  when given) for `p`. Exactly one match is required; zero or several matches
  are errors that state the count.
- Arithmetic is wrapping 64-bit; `-` exists for negative offsets.

API (sketch):

```rust
pub struct AddrExpr { /* parsed tree */ }
impl AddrExpr {
    pub fn parse(s: &str) -> Result<AddrExpr, ParseError>; // with column
    pub fn resolve(&self, ctx: &ResolveCtx) -> Result<u64, ResolveError>;
    // True when no plain number is used as an address: every base is a
    // module or aob(), numbers only appear as offsets.
    pub fn is_static(&self) -> bool;
}
impl fmt::Display for AddrExpr {} // canonical form: hex offsets, single spaces
```

`ResolveCtx` bundles the target's `Mem` and its memory map, so one resolve pass
over a whole table reads the map once. `ResolveError` names the failing step
("module \"dmhost\" not found", "pointer at 0x… unreadable", "aob(…) matched 3
times, must be unique").

The pointer scanner and the wizard produce `AddrExpr` values and format them,
so everything that ends up in a table goes through the same parser.

### 2. Table file (`cheatu-core::table`)

```toml
version = 1
process = "dmhost"            # optional

[[entry]]
description = "Rupees"
address = '[["dmhost"+0x1a2b30]+0x48]+0x6337e134'
type = "i16"
value = "200"
frozen = true
```

- `type` uses the names `ScanType` already parses (`i8` … `f64`) plus `bytes`
  for byte arrays, where `value` is hex and fixes the length.
- `version` is required. An unknown version is refused, so the scripting spec
  can extend the format safely.
- Reading and writing only (`serde` + `toml`, both MIT/Apache). No UI logic.
- Limits: files over 1 MiB and tables over 10 000 entries are refused.
- Errors carry the TOML line number. An entry whose `value` does not parse as
  its `type`, or whose `address` does not parse, fails the whole load with that
  line, so a broken table is never half-loaded.
- Save writes only what the user sees: description, expression, type, value,
  frozen. Never resolved addresses or PIDs. Writes go to a temporary file that
  is renamed over the target, so a failed save never corrupts an existing
  table.

### 3. Cheat table in the GUI and CLI

GUI (`cheatu-gui`):

- The cheat table gets **Save**, **Save as** and **Open** buttons (native file
  dialog via `rfd`, MIT).
- Each entry's address field is an editable expression. Next to it: the
  resolved address, or the error in red.
- Adding a scan result to the table: when the address falls in a module's
  image, the entry starts as `"module"+offset`; otherwise as a plain address.
- Per entry: **Make AoB signature** (section 4) and **Pointer scan for this
  address** (section 5).
- The last resolved address is kept per entry. A frozen entry is re-resolved
  about once a second and after a failed write, not on every 40 ms freeze
  tick, because pointers can change when the game loads a new area.
- An entry that does not resolve stays in the table, shows its error, is never
  written to, and is retried every few seconds.

Opening a table:

- With `process` set and nothing attached, cheatu offers to attach to the
  running process with that name; with several matches, the existing process
  picker opens with its filter set to the name.
- Attached to a process with another name: a warning, but the table opens.
- **All freezes are off after opening**, whatever the file says. The file's
  `frozen` flag is shown as "frozen when saved" until the user ticks freeze.
  Opening never writes target memory.

CLI (`cheatu-cli`), same core logic:

- `save <file>`, `load <file>` (freezes off, as in the GUI), `resolve <expr>`.
- `ptrscan <expr|addr> [depth] [max-offset]`, `ptrscan filter addr <expr|addr>`,
  `ptrscan filter value <value>`, `ptrscan save <file>`, `ptrscan load <file>`,
  `ptrscan list [n]`.

### 4. AoB signature helper

For an entry's resolved address, find the shortest byte window around it that
occurs exactly once in its module (or, outside a module, in all readable
memory), up to 64 bytes. Bytes that differ between two reads a moment apart
become `??`. The result is offered as `aob("…", "module")+offset`, offset being
the distance from the window's start to the address. When no unique window of
64 bytes exists, the helper says so and suggests the pointer scanner.

This is mostly useful for addresses inside code or static data. For heap
values the pointer scanner is the right tool; the helper's message says so.

### 5. Pointer scanner (`cheatu-core::pointer_scan`)

**Settings:** maximum depth (number of `[…]` steps, default 5), maximum offset
per step (default 0x1000), alignment (default 8), result cap (default
1 000 000).

**Pointer map.** One snapshot of the target, paused while it is taken when
"pause target while scanning" is on:

- Read every readable and writable region and record each aligned 8-byte value
  that points into a readable region, as `(value, location)`.
- Mark a location *static* when it lies inside a module's image, so that
  `"module"+offset` names it across restarts.
- A large target can hold hundreds of millions of candidates (16 bytes each),
  so the map is written to a temporary file and sorted on disk by `value` in
  chunks, then searched by binary search over a memory-mapped file
  (`memmap2`, MIT/Apache). Memory use stays bounded regardless of target size.

**Search.** Backwards from the target `T`:

1. Level 0 targets: `{T}`.
2. For every target `t`, find map entries whose `value` is in
   `[t - max_offset, t]`. Each gives offset `t - value` and a location.
3. A static location completes a chain `"module"+(location-base)` with the
   collected offsets. A non-static location becomes a target one level deeper,
   while the depth limit allows.
4. Stop at the result cap; duplicates are dropped.

Runs on a background thread with progress and cancel, like the existing scans.

**Results file** (`.cheatu-ptr`, binary): a header with format version,
process name, module names, settings, and the target address; then the chains
as (module index, base offset, offset list). Opening it lists the chains,
shortest first.

**Filtering after a restart:**

- **Filter with new address:** resolve every chain in the current process and
  keep those that land exactly on the given address (found again with a normal
  value scan or the wizard).
- **Filter by value:** keep the chains whose target currently holds the given
  value of the given type, for when the address is not known yet.
- Filtering only needs the chains and the live process, not a new pointer map.

**Add to table** turns a chain into an `AddrExpr`, e.g.
`[["dmhost"+0x1a2b30]+0x48]+0x6337e134`.

### 6. Cheat wizard (GUI)

A separate window, opened from the toolbar ("New cheat…"). The expert screens
stay as they are.

1. **Find the value.** "What value do you want to change, and what is it now?"
   The wizard runs the first scan. "Change it in the game and enter the new
   value" runs a next scan, repeated until at most 10 candidates remain. For
   values without a visible number (a health bar): "increased / decreased /
   unchanged" buttons instead of a number.
2. **Find the real one.** For each candidate, write a test value (the existing
   probe, with the user's consent) or ask "Did the value change in the game?".
   Copies that the game overwrites are dropped.
3. **Make it last.** In a module's image: done (`"module"+offset`). Otherwise:
   run the pointer scanner, then "Restart the game and come back". cheatu stays
   open, waits for a process with the same name, re-attaches, and repeats
   step 1 to find the new address, then filters the pointer results with it.
   Repeat until at most a few chains remain, or the user stops and keeps what
   is left.
4. **Save.** Name, frozen or fixed value; added to the cheat table, which the
   user can save and share.

The wizard's steps and transitions are a state machine without egui
(`cheatu-gui/src/wizard.rs`), driven by a small trait over the scanner and
process list, so it can be tested with fakes. The egui code only renders the
current state and forwards button presses.

### 7. Errors

- Parse and resolve errors are shown where the expression is (red text under
  the field in the GUI, a message in the CLI), never as a crash.
- Table load errors: status bar message with the line number; the current
  table is left untouched.
- Pointer scanner: out of disk space, or the process exits during the
  snapshot, ends the scan with a message and removes the temporary files.
- Wizard: when no candidate survives, or no chain survives a restart, the
  wizard says so and offers to start over, or to save the plain address as a
  cheat for this session only.

## Testing

All automated tests run in the existing CI jobs (Linux and macOS) without root,
against the test process's own memory, like `cheatu-core/tests/macos.rs`.

- **`addr_expr`:** parse table of valid and invalid expressions with column
  numbers; `format(parse(s))` round-trips (plus a `proptest` property test on
  random trees); resolve against the test process: a `static` gives
  `"module"+offset`, a `static → Box → Box → value` chain resolves, a unique
  pattern is found, a pattern present twice reports 2 matches.
- **`table`:** save/open round-trip with a fixture file; `frozen = true` in a
  file never starts a freeze; refusals with the right line for invalid TOML,
  unknown type, out-of-range value, too large file, unknown version.
- **`pointer_scan`:** build `static → Box → Box → value`, scan, expect the known
  chain with the right offsets; simulate a restart by freeing and rebuilding
  the structure at new heap addresses (same offsets, static updated) and filter
  with the new address: the real chain survives, unrelated ones are dropped;
  same for filter by value; depth, offset and result-cap limits; cancel; the
  disk-backed map is exercised by forcing a tiny in-memory chunk size.
- **Wizard state machine:** normal flow, value never found, no chain left after
  a restart, target process disappearing mid-flow, all with fakes.

Manual:

- **Feasibility measurement (first plan step):** pointer map of `dmhost` with
  OoT running (5.3 GiB, as root): time, disk and memory. Adjust defaults if
  needed before building the rest.
- **End-to-end, macOS as root:** with the wizard, find the rupees, make them
  last with at least two game restarts, save; quit game and cheatu; start both,
  open the table, tick freeze: rupees at 200. Open a "shared" table and check
  that nothing is written before a freeze is ticked.

## Risks

- **Pointer map size and speed** on large targets. Mitigated by the disk-backed
  map and measured first; defaults may need tuning.
- **No stable chain:** some games rebuild their state so no chain survives a
  restart. The wizard reports it and keeps the session-only option.
- **AoB patterns over data** are often not unique or change with the value. The
  helper's message points to the pointer scanner for heap values.
- **Module name collisions** (two images with the same file name) are reported,
  not guessed.

## Dependencies

New, all open source: `toml` and `serde` (MIT/Apache) for tables, `memmap2`
(MIT/Apache) for the pointer map, `rfd` (MIT) for file dialogs in the GUI,
`proptest` (MIT/Apache) as a dev-dependency.
