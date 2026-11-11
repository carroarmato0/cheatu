# Cheat tables, pointer scanner and cheat wizard Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Saveable cheat tables whose entries find their address again through address expressions, a pointer scanner that produces such expressions and narrows them across game restarts, and a GUI wizard that guides users through it.

**Architecture:** Three new, UI-agnostic modules in `cheatu-core`: `addr_expr` (parse/format/resolve expressions), `table` (TOML file format) and `pointer_scan` (disk-backed pointer map, backwards search, results file, filters), plus `signature` (AoB helper) and module lookup helpers in `maps`. Memory access in these modules goes through a small `MemSource` trait so tests can inject failures. The GUI and CLI only call into core; the wizard is a pure state machine in `cheatu-gui/src/wizard.rs` with a thin egui layer.

**Tech Stack:** Rust (stable, edition 2021), egui/eframe 0.29, new crates `serde` + `toml` + `serde_json` (core), `memmap2` (core), `rfd` (gui), `proptest` (core dev-dependency). All MIT/Apache.

**Spec:** `docs/superpowers/specs/2026-11-11-cheat-tables-pointer-scanner-design.md`

## Global Constraints

- Expression grammar, semantics and error wording as in spec section 1. Hex requires `0x`; bare digits are decimal.
- `[x]` reads 8 bytes little-endian; 64-bit targets only.
- Module names match the last path component of a region's path, exactly and case-sensitively; same name from two different paths is an error naming both.
- Table file: `version = 1` required, unknown version refused; max 1 MiB file, max 10 000 entries; any invalid entry fails the whole load with its line number; save writes only description, address, type, value, frozen; save is write-to-temp-then-rename.
- Loading a table (GUI or CLI) never writes target memory; all freezes start off.
- Pointer scan defaults: max depth 5, max offset per step 0x1000, alignment 8, result cap 1 000 000.
- AoB helper: window up to 64 bytes; bytes that differ between two reads become `??`.
- Wizard: next scans until at most 10 candidates remain.
- Frozen entries re-resolve about once a second and after a failed write; unresolved entries retry every 3 seconds.
- All new dependencies open source (MIT/Apache).
- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings` and `cargo test --workspace --locked` stay clean on macOS and Linux (see `LINUX_CHECK` in `docs/superpowers/plans/2026-11-11-macos-support-phase1.md`).
- If `cargo` is not found: `export PATH="$(brew --prefix rustup)/bin:$PATH"`.
- Every commit message ends with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Review Focus

- A frozen entry whose pointer chain stops resolving (game loads another area): no write goes to the stale address, the error shows, and it recovers when the chain resolves again. Test in Task 9 (`resolve_schedule_*`).
- A chain through a null or unmapped intermediate pointer: `resolve` returns `ResolveError::Unreadable`, never panics, and filtering simply drops the chain. Tests in Task 3 and Task 7.
- The target exits while the pointer map is being built: the scan ends with an error and no temp files remain. Test in Task 6.
- A table edited on Windows (CRLF line endings) or with a misspelt key: CRLF loads fine; an unknown key is refused with its line number. Tests in Task 4.
- Module names with spaces or non-ASCII characters (`"My Game"`, `"ゼルダ"`) parse, format and resolve; a name containing `"` is a parse error. Tests in Task 2.

---

### Task 1: Feasibility measurement of the pointer map (throwaway)

Answers one question before anything is built: how many pointer candidates does `dmhost` (OoT, ~5.3 GiB) hold, and how long does reading them take?

**Files:**
- Create (throwaway, deleted in Step 4): `crates/cheatu-core/examples/ptrmap_measure.rs`

**Interfaces:**
- Consumes: `cheatu_core::{read_maps, Mem, MemoryRegion}` (existing).

- [ ] **Step 1: Write the example.** Args: `<pid>`. For every region with `read && write`, read it in 4 MiB chunks; count aligned 8-byte values that fall inside any readable region (binary search over the sorted region starts). Print per run: total readable-writable bytes, candidate count, estimated map size (`count * 16` bytes), elapsed seconds, peak RSS (`getrusage`).
- [ ] **Step 2: Build it.** Run: `cargo build --release --example ptrmap_measure`. Expected: builds.
- [ ] **Step 3: Ask the user to run it as root** with the game running: `! sudo target/release/examples/ptrmap_measure <dmhost pid>`. Record the output in the spec under a new heading `## Measurements` (date, numbers). If the estimated map exceeds 8 GiB or reading exceeds 120 s, stop and discuss defaults (e.g. restricting regions) with the user before Task 6.
- [ ] **Step 4: Delete the example and commit the measurement.**

```bash
rm crates/cheatu-core/examples/ptrmap_measure.rs   # never committed
git add docs/superpowers/specs/2026-11-11-cheat-tables-pointer-scanner-design.md
git commit -m "docs: pointer map measurement on dmhost"
```

### Task 2: Module lookup and address-expression parser

**Files:**
- Modify: `crates/cheatu-core/src/maps.rs` (module helpers)
- Create: `crates/cheatu-core/src/addr_expr.rs` (parser, tree, `Display`; unit tests inline)
- Modify: `crates/cheatu-core/src/lib.rs` (exports), `crates/cheatu-core/Cargo.toml` (`proptest` dev-dependency)

**Interfaces:**
- Produces (maps):
  - `pub fn module_name(region: &MemoryRegion) -> Option<&str>`: last path component when `path` is non-empty and does not start with `[`.
  - `pub fn module_base(regions: &[MemoryRegion], name: &str) -> Result<u64, ModuleError>`: lowest `start` of regions whose `module_name == name`.
  - `pub fn module_containing(regions: &[MemoryRegion], addr: u64) -> Option<(String, u64)>`: (name, base) of the image containing `addr`.
  - `pub enum ModuleError { NotFound(String), Ambiguous { name: String, paths: Vec<String> } }`
- Produces (addr_expr):
  - `pub enum Expr { Num(u64), Module(String), Deref(Box<Expr>), Aob { pattern: String, module: Option<String> }, Add(Box<Expr>, Box<Expr>), Sub(Box<Expr>, Box<Expr>) }`
  - `pub struct AddrExpr(pub Expr)` with `pub fn parse(s: &str) -> Result<AddrExpr, ParseError>`, `pub fn is_static(&self) -> bool`, `impl Display` (canonical: hex offsets as `0x…` lowercase, no spaces around `+`/`-`, `aob("…", "…")`).
  - `pub struct ParseError { pub column: usize, pub message: String }` (1-based column).

- [ ] **Step 1: Write failing tests.** In `maps.rs` tests, build synthetic `MemoryRegion` lists:
  - `module_base_picks_lowest_start`: regions `/a/dmhost` at 0x2000 and 0x1000 → `Ok(0x1000)`.
  - `module_base_ambiguous_names_both_paths`: `/a/libfoo.dylib` and `/b/libfoo.dylib` → `Ambiguous` with both paths.
  - `module_base_unknown`: → `NotFound("nope")`.
  - `module_containing_returns_name_and_base`.

  In `addr_expr.rs` tests:
  - `parses_module_offset`: `"dmhost"+0x1a2b30` → `Add(Module("dmhost"), Num(0x1a2b30))`.
  - `parses_pointer_chain`: `[["dmhost"+0x1a2b30]+0x48]+0x6337e134` round-trips through `Display` unchanged.
  - `parses_aob_with_and_without_module`; `decimal_without_prefix`: `10` → `Num(10)`; `minus_offset`: `"m"-0x8`.
  - `whitespace_is_ignored`: `[ "m" + 0x10 ] + 4` formats as `["m"+0x10]+0x4`.
  - `names_with_spaces_and_unicode`: `"My Game"+0x10`, `"ゼルダ"+0x10` parse and round-trip.
  - Errors with column: `["m"+0x10` → column 9 "expected `]`"; `"m"+` → "expected a number, module, `[` or aob("; `foo(1)` → "unknown function `foo`"; `"a"b"` → error (quote in name).
  - `is_static`: true for `["m"+0x10]+0x4` and `aob("90", "m")+0x2`; false for `0x7ffe0000+0x4`.
  - `proptest`: random `Expr` trees (depth ≤ 4) satisfy `parse(format(e)) == e`.
- [ ] **Step 2: Run, expect failures.** `cargo test -p cheatu-core module_ addr_expr` → FAIL (items not defined).
- [ ] **Step 3: Implement** the helpers and a hand-written recursive-descent parser following the spec grammar. `+`/`-` are left-associative. `is_static`: an expression is static when, walking the leftmost operand chain of every `Add`/`Sub` and the inside of every `Deref`, the base is a `Module` or `Aob`, never a `Num`.
- [ ] **Step 4: Run, expect PASS.** `cargo test -p cheatu-core`.
- [ ] **Step 5: Commit** `feat(core): module lookup and address-expression parser`.

### Task 3: Resolving address expressions

**Files:**
- Modify: `crates/cheatu-core/src/addr_expr.rs` (resolve)
- Create: `crates/cheatu-core/src/source.rs` (`MemSource`)
- Create: `crates/cheatu-core/tests/addr_expr.rs` (integration tests against the test process)

**Interfaces:**
- Consumes: Task 2 items; existing `Mem`, `read_maps`, `parse_aob`.
- Produces:
  - `pub trait MemSource { fn regions(&self) -> &[MemoryRegion]; fn read(&self, addr: u64, buf: &mut [u8]) -> io::Result<usize>; }`
  - `pub struct LiveSource { mem: Mem, regions: Vec<MemoryRegion> }` with `pub fn open(pid: i32) -> io::Result<LiveSource>` and `pub fn refresh_regions(&mut self) -> io::Result<()>`; implements `MemSource`.
  - `impl AddrExpr { pub fn resolve(&self, src: &dyn MemSource) -> Result<u64, ResolveError> }`
  - `pub enum ResolveError { Module(ModuleError), Unreadable(u64), AobCount { pattern: String, count: usize }, BadPattern(String) }` with `Display` matching the spec wording ("module \"dmhost\" not found", "pointer at 0x… unreadable", "aob(…) matched 3 times, must be unique").

- [ ] **Step 1: Write failing tests** in `tests/addr_expr.rs`, with `LiveSource::open(std::process::id() as i32)`:
  - `module_offset_reaches_a_static`: a `static VALUE: AtomicU64`; `module_containing` gives (name, base); `"name"+(addr-base)` resolves to `&VALUE`.
  - `pointer_chain_resolves`: `static ROOT: AtomicPtr<Node>` → `Box<Node>` → `Box<u64>`; expression `[["name"+r]+o1]+o2` resolves to the value's address.
  - `aob_unique_match`: a heap buffer with a random 16-byte marker; `aob("<marker hex>")` resolves to its address.
  - `aob_two_matches_reports_count`: marker present twice → `AobCount { count: 2, .. }`.
  - `deref_of_null_is_unreadable`: `[0x0]+0x10` → `Unreadable(0)`; a chain whose intermediate pointer is set to null → `Unreadable(0)`, no panic.
- [ ] **Step 2: Run, expect FAIL.** `cargo test -p cheatu-core --test addr_expr`.
- [ ] **Step 3: Implement** `MemSource`, `LiveSource`, `resolve`. AoB search reads regions in 4 MiB chunks with a pattern-length overlap and stops counting at 2 matches beyond the first (enough for the error).
- [ ] **Step 4: Run, expect PASS** on macOS; run `LINUX_CHECK`.
- [ ] **Step 5: Commit** `feat(core): resolve address expressions against a live process`.

### Task 4: Table file format

**Files:**
- Create: `crates/cheatu-core/src/table.rs`, `crates/cheatu-core/tests/table.rs`, `crates/cheatu-core/tests/fixtures/rupees.toml`
- Modify: `crates/cheatu-core/Cargo.toml` (`serde` with `derive`, `toml`), `lib.rs`

**Interfaces:**
- Consumes: `AddrExpr::parse`, `ScanType` parsing (`value.rs`), `ScanType::parse(value)`.
- Produces:
  - `pub struct CheatTable { pub process: Option<String>, pub entries: Vec<TableEntry> }`
  - `pub struct TableEntry { pub description: String, pub address: AddrExpr, pub ty: ScanType, pub value: String, pub frozen: bool }` (`ty` is `type` in the file; `bytes` maps to `ScanType::Bytes(len of hex value)`).
  - `pub fn load(path: &Path) -> Result<CheatTable, TableError>`; `pub fn parse(text: &str) -> Result<CheatTable, TableError>`; `pub fn save(table: &CheatTable, path: &Path) -> io::Result<()>`; `pub fn to_toml(table: &CheatTable) -> String`.
  - `pub struct TableError { pub line: Option<usize>, pub message: String }` with `Display` "line N: message".
  - Constants `pub const MAX_FILE_BYTES: u64 = 1 << 20; pub const MAX_ENTRIES: usize = 10_000;`

- [ ] **Step 1: Write failing tests.**
  - `fixture_round_trips`: `parse(fixture)` then `to_toml` then `parse` gives an equal table; the fixture is the spec's example (`version = 1`, `process = "dmhost"`, one Rupees entry, `frozen = true`).
  - `frozen_is_read_as_saved`: `entries[0].frozen == true` (the "never start frozen" rule is enforced by GUI/CLI, Tasks 8 and 9).
  - `crlf_loads`: fixture with `\r\n` parses.
  - Refusals, each asserting `line` and message substring: unknown key `descripton` → line of that key; `type = "i33"` → "unknown type"; `type = "i8"`, `value = "300"` → "does not fit i8"; bad expression → "address:" plus the parse error; missing `version` → "version"; `version = 2` → "unsupported version 2".
  - `too_large_file_refused`: a file over 1 MiB → error without parsing.
  - `too_many_entries_refused`: 10 001 entries → error.
  - `save_is_atomic`: saving over an existing file into a read-only directory fails and leaves the old file intact; normal save then `load` round-trips.
- [ ] **Step 2: Run, expect FAIL.** `cargo test -p cheatu-core --test table`.
- [ ] **Step 3: Implement** with serde structs using `#[serde(deny_unknown_fields)]`, mapping `toml` error spans to line numbers. Entry-level checks (type, value fits, expression) report the line of the entry's `[[entry]]` header. Save: write `<path>.tmp-<pid>` then `fs::rename`.
- [ ] **Step 4: Run, expect PASS.**
- [ ] **Step 5: Commit** `feat(core): cheat table TOML format`.

### Task 5: AoB signature helper

**Files:**
- Create: `crates/cheatu-core/src/signature.rs`; tests in `crates/cheatu-core/tests/signature.rs`

**Interfaces:**
- Consumes: `MemSource`, `module_containing`, `Expr::Aob`.
- Produces: `pub fn make_signature(src: &dyn MemSource, addr: u64, max_len: usize) -> Result<AddrExpr, SignatureError>` and `pub enum SignatureError { NotUnique, Unreadable(u64) }`. Called with `max_len = 64`.

- [ ] **Step 1: Write failing tests.**
  - `unique_window_found`: a heap buffer of random bytes with the target in the middle; the result resolves (Task 3) back to `addr`.
  - `changing_bytes_become_wildcards`: a byte a background thread flips between the two reads appears as `??` in the pattern.
  - `repetitive_memory_not_unique`: target inside 4 KiB of zeros duplicated elsewhere → `NotUnique`.
- [ ] **Step 2: Run, expect FAIL.**
- [ ] **Step 3: Implement.** Read 64 bytes before and after `addr` twice, 50 ms apart; mark differing bytes as wildcards. Grow a window centred on `addr` from 8 bytes in steps of 8 up to `max_len`, returning the first that has exactly one match in the search scope (the module's regions when `module_containing` finds one, else all readable regions). Offset = `addr - window_start`.
- [ ] **Step 4: Run, expect PASS.**
- [ ] **Step 5: Commit** `feat(core): AoB signature helper`.

### Task 6: Disk-backed pointer map

**Files:**
- Create: `crates/cheatu-core/src/pointer_scan/mod.rs`, `crates/cheatu-core/src/pointer_scan/map.rs`; tests in `crates/cheatu-core/tests/pointer_map.rs`
- Modify: `crates/cheatu-core/Cargo.toml` (`memmap2`)

**Interfaces:**
- Consumes: `MemSource`, `module_containing`, `ScanProgress` (existing; reused for progress and cancel).
- Produces:
  - `pub struct PtrScanSettings { pub max_depth: usize, pub max_offset: u64, pub alignment: u64, pub max_results: usize, pub chunk_entries: usize }` with `Default` = 5, 0x1000, 8, 1_000_000, 64 * 1024 * 1024.
  - `pub struct PointerMap` with `pub fn build(src: &dyn MemSource, settings: &PtrScanSettings, progress: &ScanProgress) -> io::Result<PointerMap>`, `pub fn len(&self) -> u64`, `pub fn pointers_into(&self, lo: u64, hi: u64) -> impl Iterator<Item = (u64 /*value*/, u64 /*location*/)>`, `pub fn is_static(&self, location: u64) -> bool`. Temp files under `std::env::temp_dir()/cheatu-ptrmap-<pid>-<n>/`, removed on `Drop` and on every error path.

- [ ] **Step 1: Write failing tests.**
  - `finds_known_pointers`: in the test process, `Box<u64>` and a struct holding a pointer to it; `pointers_into(addr, addr+1)` contains the struct field's address.
  - `static_locations_marked`: a `static AtomicPtr` location → `is_static == true`; a heap location → false.
  - `small_chunks_still_sorted`: with `chunk_entries = 1024`, every `pointers_into` result is correct (forces multi-chunk merge on disk).
  - `read_failure_cleans_up`: a fake `MemSource` that errors after the first region → `build` returns `Err`, and the temp directory no longer exists.
  - `cancel_stops_and_cleans_up`: cancel before build → `Err(Interrupted)`, no temp dir.
- [ ] **Step 2: Run, expect FAIL.**
- [ ] **Step 3: Implement.** `ScanProgress`'s helpers in `scan.rs` (`cancelled`, `take_cancel` and the start/advance methods) are module-private; make them `pub(crate)` so `pointer_scan` can report progress and honour cancel. Pass 1: walk readable+writable regions in 4 MiB chunks, emit `(value, location)` pairs whose value lands in a readable region (binary search over region starts), buffer up to `chunk_entries`, sort the buffer by value, write it as a run file of 16-byte little-endian records. Pass 2: k-way merge runs into `map.bin`; mmap it. `pointers_into` binary-searches the mapped records. Keep a sorted list of module image ranges for `is_static`.
- [ ] **Step 4: Run, expect PASS** (macOS, then `LINUX_CHECK`).
- [ ] **Step 5: Commit** `feat(core): disk-backed pointer map`.

### Task 7: Pointer search, results file and filters

**Files:**
- Create: `crates/cheatu-core/src/pointer_scan/search.rs`, `crates/cheatu-core/src/pointer_scan/results.rs`; tests in `crates/cheatu-core/tests/pointer_scan.rs`
- Modify: `crates/cheatu-core/Cargo.toml` (`serde_json`), `lib.rs`

**Interfaces:**
- Consumes: `PointerMap`, `PtrScanSettings`, `AddrExpr`, `MemSource`, `ScanType`, `ScanValue`.
- Produces:
  - `pub struct PointerChain { pub module: String, pub base_offset: u64, pub offsets: Vec<u64> }` (offsets ordered from the static side outwards); `pub fn to_expr(&self) -> AddrExpr` (depth 2: `[["m"+base]+offsets[0]]+offsets[1]`); `pub fn resolve(&self, src: &dyn MemSource) -> Result<u64, ResolveError>`.
  - `pub fn find_chains(map: &PointerMap, src: &dyn MemSource, target: u64, settings: &PtrScanSettings, progress: &ScanProgress) -> io::Result<Vec<PointerChain>>`, sorted shortest first, then by module and offsets.
  - `pub struct PtrResults { pub process: String, pub target: u64, pub settings: PtrScanSettings, pub chains: Vec<PointerChain> }` with `pub fn save(&self, path: &Path) -> io::Result<()>`, `pub fn load(path: &Path) -> io::Result<PtrResults>`, `pub fn filter_by_address(&mut self, src: &dyn MemSource, addr: u64) -> usize` and `pub fn filter_by_value(&mut self, src: &dyn MemSource, ty: ScanType, value: &ScanValue) -> usize` (both return how many remain).
  - File format: magic `b"CHEATUPTR"`, `u32` version 1, `u32` header length, JSON header (`process`, `target`, `settings`, `modules: Vec<String>`), then per chain: `u32` module index, `u64` base offset, `u8` depth, depth × `u64` offset; little-endian.

- [ ] **Step 1: Write failing tests.**
  - `finds_known_chain`: `static ROOT → Box<A> (ptr at +0x18) → Box<B> (value at +0x40)`; scan from the value's address with defaults; results contain module = test binary, offsets `[0x18, 0x40]`.
  - `respects_depth_and_offset_limits`: with `max_depth = 1` the depth-2 chain is absent; with `max_offset = 0x20` a chain needing 0x40 is absent.
  - `result_cap`: `max_results = 3` → at most 3 chains.
  - `restart_simulation_keeps_real_chain`: scan, then drop and rebuild the structure at new heap addresses with the same offsets and `ROOT` updated; `filter_by_address(new_addr)` keeps the real chain and drops chains that only matched by coincidence (assert the real one survives and the count shrinks or stays ≥ 1).
  - `filter_by_value_works`: same structure, value set to 4242; `filter_by_value(I32, 4242)` keeps the real chain.
  - `null_intermediate_dropped`: a chain whose middle pointer is nulled is removed by `filter_by_address`, no panic.
  - `results_file_round_trips`: save then load gives equal `PtrResults`; a file with a wrong magic → `InvalidData`.
- [ ] **Step 2: Run, expect FAIL.**
- [ ] **Step 3: Implement** `find_chains` as a breadth-first search: level targets start as `{target}`; for each target `t`, `map.pointers_into(t.saturating_sub(max_offset), t + 1)` gives `(value, location)` with offset `t - value`; a static location completes a chain (`module_containing(location)`), otherwise the location becomes a target at the next level while `level < max_depth`. Track visited `(location, level)` to drop duplicates; stop at `max_results`; check `progress` for cancel between targets.
- [ ] **Step 4: Run, expect PASS** (macOS, then `LINUX_CHECK`).
- [ ] **Step 5: Commit** `feat(core): pointer chain search, results file and filters`.

### Task 8: CLI commands

**Files:**
- Modify: `crates/cheatu-cli/src/main.rs` (`Repl` gains `table: Vec<TableEntry>`, `ptr: Option<PtrResults>`, new commands, help text)

**Interfaces:**
- Consumes: `table::{load, save}`, `AddrExpr`, `LiveSource`, `PointerMap`, `find_chains`, `PtrResults`.
- Produces commands (help text one line each):
  - `add <expr> [type] [description…]`, `entries` (each with resolved address or error, and current value), `save <file>`, `load <file>` (prints "loaded N entries, freezes off"), `resolve <expr>`.
  - `write <expr|hex> <value>`: an argument made only of hex digits keeps today's meaning (hex, `0x` optional); anything else is parsed as an expression.
  - `ptrscan <expr|hex> [depth] [max-offset]`, `ptrscan filter addr <expr|hex>`, `ptrscan filter value <value>` (uses the current type), `ptrscan save <file>`, `ptrscan load <file>`, `ptrscan list [n]` (default 20).

- [ ] **Step 1: Write failing tests.** Factor command dispatch so it can be driven with a `Vec<u8>` writer; tests in `crates/cheatu-cli/src/main.rs` `#[cfg(test)]` against the test process (`attach <own pid>`):
  - `add_and_entries_show_resolved_value`, `save_then_load_round_trips_with_freezes_off`, `resolve_reports_error_text` (unknown module message), `write_accepts_legacy_hex` (`write 7ffe…` still treated as hex), `ptrscan_finds_chain_and_lists_it`.
- [ ] **Step 2: Run, expect FAIL.** `cargo test -p cheatu-cli`.
- [ ] **Step 3: Implement.** Long `ptrscan` runs print progress on one line like existing scans; Ctrl-C handling as in existing scans.
- [ ] **Step 4: Run, expect PASS**; manual: `cargo run -p cheatu-cli`, `help` lists the new commands.
- [ ] **Step 5: Commit** `feat(cli): cheat tables, expressions and pointer scan commands`.

### Task 9: GUI cheat table with expressions, save and open

**Files:**
- Create: `crates/cheatu-gui/src/cheat_table.rs` (entry state, resolve schedule, save/open glue)
- Modify: `crates/cheatu-gui/src/main.rs` (replace `SavedEntry`, panel UI, freeze feed), `crates/cheatu-gui/Cargo.toml` (`rfd`)

**Interfaces:**
- Consumes: `TableEntry`, `table::{load, save}`, `AddrExpr`, `LiveSource`, `module_containing`, existing `FreezeShared { pid, items: Vec<(u64, ScanValue)> }`.
- Produces:
  - `pub struct GuiEntry { pub entry: TableEntry, pub expr_text: String, pub resolved: Result<u64, String>, pub last_resolve: Instant, pub frozen_when_saved: bool }`
  - `pub fn due_for_resolve(e: &GuiEntry, now: Instant, write_failed: bool) -> bool`: true when `write_failed`, or `resolved` is `Err` and ≥ 3 s passed, or `entry.frozen` and ≥ 1 s passed.
  - `pub fn entry_for_address(regions: &[MemoryRegion], addr: u64, ty: ScanType) -> TableEntry`: `"module"+offset` when `module_containing` finds one, else a plain address.
  - `pub fn open_table(path: &Path) -> Result<Vec<GuiEntry>, TableError>`: every `entry.frozen` set to false, `frozen_when_saved` keeps the file's value.

- [ ] **Step 1: Write failing tests** in `cheat_table.rs`:
  - `resolve_schedule_frozen_every_second`, `resolve_schedule_failed_every_three_seconds`, `resolve_schedule_immediately_after_failed_write`.
  - `stale_entry_not_fed_to_freeze`: a frozen entry whose `resolved` is `Err` contributes nothing to the freeze item list (factor `fn freeze_items(entries: &[GuiEntry]) -> Vec<(u64, ScanValue)>` and test it).
  - `opened_table_starts_unfrozen`: fixture with `frozen = true` → all `entry.frozen == false`, `frozen_when_saved == true`.
  - `entry_for_static_address_uses_module`.
- [ ] **Step 2: Run, expect FAIL.** `cargo test -p cheatu-gui`.
- [ ] **Step 3: Implement** and wire into `main.rs`:
  - Cheat table header gets **Save**, **Save as**, **Open** (`rfd::FileDialog`, filter `*.toml`).
  - Each row: description, editable expression (re-parse on edit; parse error in red below), resolved address or error in red, type, value, Set, delete, freeze; "frozen when saved" hint when applicable.
  - Opening with `process` set: not attached → offer to attach (single match) or open the picker with `proc_filter` set to the name; attached to another name → warning in the status bar.
  - The "+" on a scan result uses `entry_for_address`.
- [ ] **Step 4: Run, expect PASS**; manual: save a table, reopen it, freezes off, resolved addresses shown.
- [ ] **Step 5: Commit** `feat(gui): cheat tables with address expressions, save and open`.

### Task 10: GUI AoB action and pointer-scan window

**Files:**
- Create: `crates/cheatu-gui/src/pointer_window.rs`
- Modify: `crates/cheatu-gui/src/main.rs` (row actions, window state)

**Interfaces:**
- Consumes: `make_signature`, `PointerMap::build`, `find_chains`, `PtrResults`, `GuiEntry`.
- Produces: `pub struct PointerWindow` with `pub fn open_for(target: u64, process: String) -> Self`, `pub fn show(&mut self, ctx: &egui::Context, src: Option<&LiveSource>) -> Option<TableEntry>` (returns an entry when the user clicks **Add to table**).

- [ ] **Step 1: Write failing test** for the one non-UI piece: `fn chain_rows(results: &PtrResults, limit: usize) -> Vec<String>` formats rows as the chain's expression, shortest first, at most `limit` (1000).
- [ ] **Step 2: Run, expect FAIL.**
- [ ] **Step 3: Implement.**
  - Row action **Make AoB signature**: replaces the expression with the helper's result; on `NotUnique` the status bar says "No unique pattern within 64 bytes; for heap values use Pointer scan".
  - Row action **Pointer scan**: opens `PointerWindow`. Settings fields (depth, max offset, alignment, result cap) with spec defaults; **Scan** runs map build + search on a background thread with progress and Cancel (pause target if the existing "pause target while scanning" is on).
  - Results: count, `chain_rows`, **Filter with new address** (expression field), **Filter by value** (value field + current type), **Save results…**, **Open results…**, **Add to table**.
- [ ] **Step 4: Run, expect PASS**; manual check against the test process or a game.
- [ ] **Step 5: Commit** `feat(gui): AoB signature action and pointer scan window`.

### Task 11: Wizard state machine

**Files:**
- Create: `crates/cheatu-gui/src/wizard.rs` (no egui imports)

**Interfaces:**
- Consumes: `ScanType`, `ScanValue`, `NextScan`, `PointerChain`, `TableEntry`, `AddrExpr`.
- Produces:
  - `pub trait WizardBackend { fn first_scan(&mut self, ty: ScanType, value: &str) -> io::Result<usize>; fn next_scan(&mut self, cmp: NextScan, value: Option<&str>) -> io::Result<usize>; fn candidates(&self) -> Vec<u64>; fn probe(&mut self, addr: u64) -> io::Result<ProbeOutcome>; fn module_offset(&self, addr: u64) -> Option<AddrExpr>; fn pointer_scan(&mut self, addr: u64) -> io::Result<usize>; fn filter_chains(&mut self, addr: u64) -> io::Result<usize>; fn chains(&self) -> Vec<PointerChain>; fn process_running(&self, name: &str) -> Option<i32>; fn reattach(&mut self, pid: i32) -> io::Result<()>; }`
  - `pub enum Step { AskValue, Narrowing { count: usize }, Verify { remaining: Vec<u64> }, MakeLasting, WaitRestart { process: String }, Refind { count: usize }, Save { expr: AddrExpr }, Failed(String) }`
  - `pub enum Input { Value(String), Changed(NextScan), Confirm(bool), Restarted, Stop, StartOver }`
  - `pub struct Wizard { pub step: Step, /* … */ }` with `pub fn new(ty: ScanType) -> Self` and `pub fn handle(&mut self, input: Input, backend: &mut dyn WizardBackend)`.

- [ ] **Step 1: Write failing tests** with a fake backend (scripted counts and candidates):
  - `normal_flow_module_address`: value → narrowing → ≤ 10 candidates → verify → module address → `Save` with `"m"+off`.
  - `normal_flow_with_restart`: heap address → `MakeLasting` → pointer scan → `WaitRestart` → `Restarted` (backend reports the process again) → `Refind` → value found → `filter_chains` → ≤ 3 chains → `Save` with the first chain's expression.
  - `value_never_found`: first scan returns 0 → `Failed` with a message and `StartOver` returns to `AskValue`.
  - `no_chain_survives`: `filter_chains` returns 0 → `Failed` offering the session-only plain address.
  - `process_disappears`: `process_running` returns `None` during `WaitRestart` → stays waiting; a backend error during `Refind` → `Failed`.
  - `invalid_value_stays_on_step`: `Value("abc")` for `I16` → still `AskValue`, with an error message; `Value("40000")` for `I16` → same.
- [ ] **Step 2: Run, expect FAIL.**
- [ ] **Step 3: Implement** the transitions exactly as tested; narrowing stops at ≤ 10 candidates; restart filtering stops at ≤ 3 chains or on `Stop`.
- [ ] **Step 4: Run, expect PASS.**
- [ ] **Step 5: Commit** `feat(gui): cheat wizard state machine`.

### Task 12: Wizard UI, docs and end-to-end check

**Files:**
- Create: `crates/cheatu-gui/src/wizard_ui.rs` (egui rendering + `WizardBackend` impl over `Scanner`, `LiveSource`, pointer scan)
- Modify: `crates/cheatu-gui/src/main.rs` (toolbar button "New cheat…", window state)
- Modify: `docs/usage.md` (sections "Cheat tables", "Address expressions", "Pointer scanner", "Cheat wizard"), `README.md` (one-line feature mention)

**Interfaces:**
- Consumes: `Wizard`, `Step`, `Input`, `WizardBackend` (Task 11); `GuiEntry` (Task 9); `PointerWindow` internals are not reused, only core calls.

- [ ] **Step 1: Implement** the window: one panel per `Step` with the question, input field or buttons (increased/decreased/unchanged for unknown values), progress for long steps, and a final form (name, frozen or fixed value) that adds the entry to the cheat table. `WaitRestart` polls `list_processes()` every 2 s for the saved name.
- [ ] **Step 2: Docs.** Usage sections with the spec's examples; mention that opening a table never writes memory.
- [ ] **Step 3: Full check.** `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked`, `LINUX_CHECK`. Expected: all clean.
- [ ] **Step 4: Manual end-to-end (macOS, as root, OoT)** per the spec: wizard finds the rupees, at least two game restarts, save; restart game and cheatu, open the table, tick freeze → 200 rupees. Open a "shared" table and confirm nothing is written before a freeze is ticked. Ask the user to confirm what the game shows; record the result in the PR description.
- [ ] **Step 5: Commit** `feat(gui): cheat wizard window` and `docs: cheat tables, pointer scanner and wizard`.
