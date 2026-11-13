# GUI Revamp Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make cheatu-gui guided by default (game list → guided steps → My cheats) with an Advanced switch that shows the full scanner and tools, plus RPG Maker auto-detection, inline feedback, safety nets and one vocabulary.

**Architecture:** `update()` in `main.rs` keeps the panels, but what the central panel draws is decided by a pure `screen::screen(&ScreenState) -> Screen`. New focused modules take over pieces of `main.rs`: `settings.rs` (config.json), `rpg_detect.rs`, `screen.rs`, `start_screen.rs`, `feedback.rs`, `my_cheats.rs`. The wizard's state machine (`wizard.rs`) is untouched; `wizard_ui.rs` gains an in-panel renderer.

**Tech Stack:** Rust, egui/eframe 0.29, cheatu-core, cheatu-inject.

**Spec:** `docs/superpowers/specs/2026-11-13-gui-revamp-design.md` (mockups: `.superpowers/brainstorm/93666-1794584084/content/{start-screen-v2,attached,my-cheats}.html`, local only)

## Global Constraints

- Vocabulary (user-visible, basic view): **game** (list: "program"), **My cheats** (Advanced: **Cheat table**), **cheat**, **Freeze**, **keeps working after a restart** / **this session only**, **Test** (Advanced: **🧪 Probe**), **needs admin rights**, **Search** / **Narrow down**. Technical words (address, expression, pointer chain, AoB, pid) only in Advanced view.
- `config.json` keys: `"theme"` (`system|dark|light`), `"advanced"` (bool, default false), `"probe"` (bool, default false). Missing/unknown keys → defaults; a theme-only file stays valid; corrupt JSON → all defaults.
- Header right side: **✨ Guided…** (Advanced only), **Advanced** switch, ⚙, ⓘ. Removed: mode tabs, "Select process…", "New cheat…", "Request root access", "🔒 Elevate & attach", privilege label.
- Inline feedback: confirmations ~3 s next to the causing control; errors stay until dismissed (×). Status bar keeps progress + Cancel + last message.
- "Survives a restart" = expression contains a dereference, `aob(…)`, or `AddrExpr::is_static()`; otherwise "this session only".
- About text: "A memory scanner and cheat tool for games on Linux and macOS, including Wine/Proton and RPG Maker titles."
- Never launch cheatu-gui with input/clicks; screenshots only via `--attach <pid>` against the testbed, with `HOME` pointed at a scratch dir when a config is needed (never the user's config).
- macOS cargo: `export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH"`. Linux: podman `rust:1`, `CARGO_TARGET_DIR=/work/target/linux`.
- Every task ends green: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked` (macOS; Linux container at Tasks 2, 6, 11).

## Review Focus

1. **Leaving the guided view mid-job** (Detach, Change game, Advanced on, game exits) must shut the wizard down and leave the game un-paused. Pinned by `wizard_runs_only_on_guided_screen` (Task 3) and the shutdown wiring in Task 6.
2. **Quitting with unsaved cheats** must ask once; "Don't save" quits, "Cancel" keeps the window and everything running. Pinned by `close_decision_*` (Task 10).
3. **Wine paths with spaces** (`C:\Program Files\Sakura\Game.exe`) and `Z:\` paths detect RPG Maker. Pinned by `wine_path_with_spaces_detects` (Task 2).
4. **An opened table of plain addresses** shows "this session only", never an error. Pinned by `plain_number_is_session_only` (Task 9).
5. **An old or corrupt config.json** loads defaults without losing the theme where readable. Pinned by `old_theme_only_config_loads` and `corrupt_config_gives_defaults` (Task 1).

---

### Task 1: settings.rs — config.json with theme, advanced, probe

**Files:**
- Create: `crates/cheatu-gui/src/settings.rs`
- Modify: `crates/cheatu-gui/src/main.rs` (move `config_path`, `load_theme*`, `save_theme*`, `give_config_to_home_owner` and their tests into `settings.rs`; `CheatuApp` gets `advanced: bool`; `enable_probe` loads/saves)

**Interfaces:**
- Produces: `pub struct Settings { pub theme: ThemePref, pub advanced: bool, pub probe: bool }` (`Default`: System, false, false); `pub fn config_path() -> PathBuf`; `pub fn load_from(path: &Path) -> Settings`; `pub fn load() -> Settings`; `pub fn save_to(path: &Path, home: &Path, s: &Settings) -> io::Result<()>`; `pub fn save(s: &Settings) -> io::Result<()>`; `pub fn give_config_to_home_owner(path: &Path, home: &Path) -> io::Result<()>` (moved unchanged).

- [ ] **Step 1: Tests** in `settings.rs` (move the three existing config tests and the macOS path test, adapted):

```rust
// settings_round_trip: save_to then load_from == Settings{theme: Light, advanced: true, probe: true}
// old_theme_only_config_loads: file `{"theme":"light"}` → Settings{Light, false, false}
// corrupt_config_gives_defaults: file `{not json` → Settings::default()
// unknown_keys_are_ignored: `{"theme":"dark","future":1}` → theme Dark, others default
// unwritable_config_reports_an_error, root_save_gives_config_to_home_owner: as today
```

- [ ] **Step 2: Run** `cargo test -p cheatu-gui settings::` → FAIL (module missing).
- [ ] **Step 3: Implement**; `CheatuApp` loads `Settings` once at startup; the Settings window saves the whole `Settings` when theme or probe changes (error → status as today). `advanced` has no UI yet (Task 3).
- [ ] **Step 4: Run** full gate → PASS.
- [ ] **Step 5: Commit** `feat(gui): settings.rs; remember Advanced and Enable probe`.

---

### Task 2: rpg_detect.rs — process → game folder → RPG Maker engine

**Files:**
- Create: `crates/cheatu-gui/src/rpg_detect.rs`
- Modify: `crates/cheatu-core/src/process.rs` (add `pub fn env_var(pid: i32, key: &str) -> Option<String>` over `sys::environ`)

**Interfaces:**
- Produces: `pub fn wine_to_unix(path: &str, prefix: Option<&Path>) -> Option<PathBuf>` (`Z:\a\b` → `/a/b`; other letters via `<prefix>/dosdevices/<x>:` symlink target, `None` without prefix); `pub fn program_dir(cmdline: &str, prefix: Option<&Path>) -> Option<PathBuf>` (tries argv0 prefixes of growing token count — `tokens[..k].join(" ")` — first that names an existing file, Wine-translated when it looks like `X:\…`; returns its parent); `pub fn detect_game(cmdline: &str, prefix: Option<&Path>) -> Option<Engine>` (walk up from `program_dir` at most 3 levels, first `detect_dir(..).is_rpgmaker()`); `pub struct Detections` cache: `pub fn get(&mut self, p: &ProcInfo) -> Option<Engine>` keyed by pid (prefix from `env_var(pid, "WINEPREFIX")`).

- [ ] **Step 1: Tests** (temp dirs with `js/rmmz_core.js` / `www/js/rpg_core.js` and an empty `Game.exe`):

```rust
// unix_path_detects_mz: "<tmp>/Sakura/Game --flag" → Some(RpgMakerMZ)
// walks_up_three_levels: exe in <tmp>/G/bin/x64/nw, js in <tmp>/G → MV; four levels up → None
// z_drive_translates: wine_to_unix("Z:\\home\\u\\G\\Game.exe", None) == Some("/home/u/G/Game.exe")
// c_drive_needs_prefix: wine_to_unix("C:\\G\\Game.exe", None) == None; with prefix whose dosdevices/c: → <tmp>/drive_c → Some(<tmp>/drive_c/G/Game.exe)
// wine_path_with_spaces_detects: "C:\\Program Files\\Sakura\\Game.exe" with prefix → Some(RpgMakerMZ)
// not_a_game: "/bin/ls -la" → None
```

- [ ] **Step 2: Run** `cargo test -p cheatu-gui rpg_detect::` → FAIL.
- [ ] **Step 3: Implement.** File checks only; no process spawning.
- [ ] **Step 4: Run** full gate, macOS and Linux container → PASS.
- [ ] **Step 5: Commit** `feat(gui): detect RPG Maker games from a running process`.

---

### Task 3: screen.rs + header

**Files:**
- Create: `crates/cheatu-gui/src/screen.rs`
- Modify: `crates/cheatu-gui/src/main.rs` (replace `AppMode` and the header block at the `// ---- Header` section; central panel dispatches on `Screen`)

**Interfaces:**
- Produces: `pub enum Screen { ChooseGame, RpgChoice, Guided, Scanner, RpgMaker }`; `pub enum RpgPick { Undecided, RpgMaker, Memory }`; `pub struct ScreenState { pub attached: bool, pub rpg: Option<Engine>, pub pick: RpgPick, pub advanced: bool, pub rpg_setup: bool }` (`rpg_setup` = the "Set it up…" link was used with no game); `pub fn screen(s: &ScreenState) -> Screen`; `pub fn wizard_runs(screen: Screen) -> bool` (true only for `Guided`). `CheatuApp` gets `rpg_pick: RpgPick`, `rpg_setup: bool`, `detections: rpg_detect::Detections`.
- Rules: not attached → `RpgMaker` if `rpg_setup` else `ChooseGame`; attached + `rpg.is_some()` + `Undecided` → `RpgChoice`; `RpgMaker` pick → `RpgMaker`; otherwise `Scanner` if `advanced` else `Guided`.

- [ ] **Step 1: Tests** `screen_selection_covers_every_state` (table of the rules above) and `wizard_runs_only_on_guided_screen`.
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement** `screen.rs`; header per Global Constraints: `cheatu`, then when attached the game name with ▾ menu (**Change game…** → detach and show `ChooseGame`; **Detach**; for RPG Maker games **RPG Maker mode** / **Memory scanner** setting `rpg_pick`); right side **✨ Guided…** (Advanced only: sets `advanced=false`), **Advanced** toggle (saves settings), ⚙, ⓘ. Until Tasks 4–6 land, `ChooseGame` draws today's picker content inline, `Guided` draws a placeholder label "Guided steps — coming in this branch", `RpgChoice` draws two buttons.
- [ ] **Step 4: Run** full gate → PASS.
- [ ] **Step 5: Commit** `feat(gui): screen model and a quieter header with Advanced switch`.

---

### Task 4: start_screen.rs — the game list is the start screen

**Files:**
- Create: `crates/cheatu-gui/src/start_screen.rs`
- Modify: `crates/cheatu-gui/src/main.rs` (picker window body → shared widget; `ChooseGame` draws the start screen; elevation becomes inline)

**Interfaces:**
- Consumes: `rpg_detect::Detections`, `pick_best_process`, `ProcInfo`.
- Produces: `pub struct GameRow { pub pid: i32, pub name: String, pub rss: u64, pub wine: bool, pub engine: Option<Engine>, pub locked: bool, pub helpers: usize }`; `pub fn fold(procs: &[ProcInfo], engines: &dyn Fn(&ProcInfo) -> Option<Engine>) -> Vec<GameRow>` (NW.js/Chromium apps: one row per program = its largest `renderer`, helpers counted; others one row each); `pub enum ListAction { Attach(i32), OpenTable, RpgSetup }`; `pub fn game_list(ui: &mut Ui, list: &mut ListState, rows: &[GameRow], advanced: bool) -> Option<ListAction>`. Advanced shows every process with PID and Role (no folding), as today.
- Copy: step bar "1 Choose your game · 2 Find a value · 3 Keep it"; heading "Which game do you want to cheat in?"; "Start the game first. The most likely one is already selected."; filter hint "Filter by name…"; tags **Wine**, **RPG Maker MV/MZ**, "🔒 needs admin rights"; buttons **Attach**, **📂 Open a saved cheat table…**; check box "Wine/Proton only"; link "RPG Maker game not running yet? Set it up…". The renderer/NW.js tip is removed.
- Admin rights: Attach on a locked row or a `PermissionDenied` attach shows inline "This game needs admin rights. **Ask for them**" (runs today's elevate-and-reattach for that pid).

- [ ] **Step 1: Tests** `fold_merges_nwjs_helpers_into_one_row` (main + 2 renderers + gpu → one row, pid = larger renderer, helpers = 3), `fold_keeps_ordinary_processes`, `best_guess_is_preselected` (uses `pick_best_process` over folded rows).
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement**; the "several instances" picker window (opening a table) reuses `game_list`.
- [ ] **Step 4: Run** full gate → PASS. Screenshot `cheatu-gui` (no `--attach`) and compare with `start-screen-v2.html` left panel.
- [ ] **Step 5: Commit** `feat(gui): the game list is the start screen`.

---

### Task 5: RPG Maker choice screen

**Files:**
- Modify: `crates/cheatu-gui/src/main.rs` (draw `RpgChoice`; prefill `RpgState` game folder from detection)

**Interfaces:**
- Consumes: `Screen::RpgChoice`, `RpgPick`, `rpg_detect::program_dir`.
- Copy: heading "{name} is an RPG Maker {MV|MZ} game", "cheatu can change it in two ways."; card **RPG Maker mode** (RECOMMENDED) bullets "Gold, items, party HP/MP, switches" · "Through the game's own code: nothing corrupts" · "One-time setup, then restart the game", button **Use RPG Maker mode**; card **Memory scanner** bullets "Any number you can see" · "Works right now, no restart" · "Cheats may break after an update", button **Scan memory**; footnote "Switch later from the game's name in the header (▾)."

- [ ] **Step 1: Test** `rpg_folder_prefill_uses_detected_dir` (pure helper `pub fn rpg_folder_for(cmdline, prefix) -> Option<PathBuf>` returning the dir where `detect_dir` matched).
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4:** gate → PASS.
- [ ] **Step 5: Commit** `feat(gui): offer RPG Maker mode for detected RPG Maker games`.

---

### Task 6: guided steps in the main area

**Files:**
- Modify: `crates/cheatu-gui/src/wizard_ui.rs` (add `pub fn show_in(&mut self, ui: &mut egui::Ui) -> Vec<WizardEvent>` sharing the body with `show`; friendly type labels only; copy per vocabulary; after `Save` the view offers **Find another value** = Start over)
- Modify: `crates/cheatu-gui/src/main.rs` (create the wizard when the screen becomes `Guided`, `shutdown()` it when it stops being `Guided`; remove the floating "New cheat" window)

**Interfaces:**
- Consumes: `screen::wizard_runs`.
- Produces: `WizardWindow::show_in`; `pub fn kind_label(t: ScanType) -> String` returns `t.friendly_label()` only.
- Copy: step bar "✓ 1 Game · 2 Find a value · 3 Keep it" (step 3 for Verify/MakeLasting/WaitRestart/Save); step 2 opener "What value do you want to change?" / "Type the number exactly as the game shows it, e.g. your rupees."; links "Search for an unknown value" and "Already know how this works? **Scan manually**" (sets Advanced on).

- [ ] **Step 1: Tests** `kind_label_has_no_rust_type_names` (no `i32`/`f32`… in any label), `step_bar_position` (pure `pub fn guided_step(step: &Step) -> u8` → 2 for AskValue/Narrowing, 3 for the rest).
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.**
- [ ] **Step 4: Run** gate on macOS and Linux container → PASS. Screenshot with `--attach <testbed pid>` (Advanced off) vs `attached.html` option A.
- [ ] **Step 5: Commit** `feat(gui): guided steps in the main area`.

---

### Task 7: feedback.rs — inline confirmations and errors

**Files:**
- Create: `crates/cheatu-gui/src/feedback.rs`

**Interfaces:**
- Produces: `pub struct Feedback`; `pub fn ok(&mut self, at: egui::Rect, text: impl Into<String>, now: Instant)`; `pub fn error(&mut self, at: egui::Rect, text: impl Into<String>, now: Instant)`; `pub fn show(&mut self, ctx: &egui::Context, now: Instant)` (draws `egui::Area`s below `at`; error has ×); `pub fn live(&self, now: Instant) -> usize`. `OK_TTL = 3 s`.

- [ ] **Step 1: Tests** `ok_expires_after_three_seconds`, `errors_stay_until_dismissed` (pure `dismiss(i)`), `same_text_same_place_replaces_instead_of_stacking`.
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4:** gate → PASS.
- [ ] **Step 5: Commit** `feat(gui): inline feedback`.

---

### Task 8: scanner (Advanced on)

**Files:**
- Modify: `crates/cheatu-gui/src/main.rs` (scanner block)

**Interfaces:**
- Consumes: `Feedback`.
- Changes: buttons **Search** / **Narrow down** (were First scan / Next scan); comparison, Narrow down and Undo shown only after a first search; `scan_blocker` reason shown as small text under the buttons; 🧪 probe available whenever Advanced is on (Settings' "Enable probe" still governs the guided view's Test); "+" on a result already in the table (same address and type) → `feedback.ok(.., "Already in My cheats")`, a new one → "✓ Added to My cheats".

- [ ] **Step 1: Tests** `narrow_controls_hidden_before_first_search` (pure `pub fn show_narrow(has_scanned: bool) -> bool`), `duplicate_add_is_detected` (pure `pub fn already_saved(saved: &[GuiEntry], addr: u64, ty: ScanType) -> bool`), update `scan_blocker_tracks_what_the_inputs_allow` for the new wording.
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4:** gate → PASS.
- [ ] **Step 5: Commit** `feat(gui): scanner wording, inline reasons, duplicate feedback`.

---

### Task 9: my_cheats.rs — basic and advanced rows, dirty tracking

**Files:**
- Create: `crates/cheatu-gui/src/my_cheats.rs` (move the side-panel body from `main.rs`'s `// ---- Cheat table` section)
- Modify: `crates/cheatu-gui/src/main.rs`

**Interfaces:**
- Consumes: `GuiEntry`, `Feedback`, `open_pointer_window`, `start_signature`.
- Produces: `pub fn survives_restart(e: &AddrExpr) -> bool`; `pub enum MakeLasting { PointerScan, Signature }` and `pub fn make_lasting_tool(regions: &[MemoryRegion], addr: u64) -> MakeLasting` (module image → Signature, else PointerScan); `pub struct Saved { snapshot: Option<Vec<TableEntry>> }` with `pub fn mark(&mut self, entries: &[GuiEntry])`, `pub fn is_dirty(&self, entries: &[GuiEntry]) -> bool` (no snapshot + empty = clean); `pub enum RowAction { Set(usize), Remove(usize), Rename(usize), MakeLasting(usize), PointerScan(usize), Signature(usize), Reresolve(usize), ChangeType(usize, ScanType) }`; `pub fn panel(ui, entries: &mut [GuiEntry], advanced: bool, ...) -> Vec<RowAction>`.
- Copy: title **My cheats** / **Cheat table** (Advanced), file name, **•** when dirty; buttons 📂 Open, 💾 Save, (Advanced) Save as…, ⋯ (Save as…, Clear all…); empty "Nothing yet. Cheats you find land here."; basic row: **❄ Freeze** toggle, name, value, **Set**, "⚠ this session only", ▸/▾; details "✓ Keeps working after a restart" / "⚠ Works until the game restarts." + **Make it last…**, "Now {v} in the game · {type}", **Rename**, **Remove…**; advanced row: ❄, name, type, value, Set, ⋯ (Change type…), details always open with expression → "→ 0x… = v", **🎯 Pointer scan…**, **🧬 AoB signature**, **⟳ Re-resolve**, **🗑**. Panel default width fits content, min 260 px, until it has rows.

- [ ] **Step 1: Tests** `pointer_chain_survives`, `aob_survives`, `module_offset_survives`, `plain_number_is_session_only`, `dirty_after_edit_clean_after_save`, `editing_back_to_saved_is_clean`, `new_empty_table_is_clean`, `make_lasting_picks_signature_for_module_data`.
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement**; wizard and pointer-window additions and Open/Save call `Saved::mark` appropriately.
- [ ] **Step 4: Run** gate → PASS.
- [ ] **Step 5: Commit** `feat(gui): My cheats with basic and advanced rows`.

---

### Task 10: safety nets

**Files:**
- Modify: `crates/cheatu-gui/src/my_cheats.rs`, `crates/cheatu-gui/src/main.rs`

**Interfaces:**
- Produces: `pub enum Pending { Quit, Open, ChangeGame, ClearAll }`; `pub enum Answer { Save, DontSave, Cancel }`; `pub fn close_decision(dirty: bool, answer: Option<Answer>) -> CloseAction` with `pub enum CloseAction { Close, Ask, Stay, SaveThenClose }`. Window close: `ctx.input(|i| i.viewport().close_requested())` + `ViewportCommand::CancelClose` while asking.
- Copy: "Save changes to {file}?" (never saved: "Save your cheats?") **Save** / **Don't save** / **Cancel**; "Remove "{name}"?" **Remove** / **Cancel**; "Remove all {n} cheats?" **Remove all** / **Cancel**.

- [ ] **Step 1: Tests** `close_decision_clean_closes`, `close_decision_dirty_asks`, `close_decision_cancel_stays`, `close_decision_save_saves_then_closes`, `close_decision_dont_save_closes`.
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement** (a failed Save keeps the window open and shows the error).
- [ ] **Step 4: Run** gate → PASS.
- [ ] **Step 5: Commit** `feat(gui): ask before losing unsaved cheats; confirm removals`.

---

### Task 11: vocabulary sweep, About, docs, screenshots

**Files:**
- Modify: all `crates/cheatu-gui/src/*.rs` string literals per Global Constraints; `main.rs` About text
- Create: `crates/cheatu-gui/tests/vocabulary.rs`
- Modify: `docs/usage.md` (GUI section rewritten for the new flow), `docs/testbed.md` (exercise steps and labels), `README.md` if it names GUI controls

**Interfaces:**
- Test reads `src/*.rs`, collects string literals outside `#[cfg(test)]` modules, and fails on: "First scan", "Next scan", "saved addresses", "Select process", "Request root access", "limited privileges", "New cheat", "Elevate & attach"; and on "pid" / "process" in literals not on the allowlist (`advanced` process list labels, `format!` debug in tests, status messages that only appear with Advanced on — each allowlisted line commented).

- [ ] **Step 1: Write** `vocabulary.rs` → **Run** → FAIL on current literals.
- [ ] **Step 2: Sweep** strings → PASS.
- [ ] **Step 3: Docs** updated; every label named in docs grepped against the source.
- [ ] **Step 4: Screenshots** (scratch `HOME` with a config for Advanced on/off) of: start screen, guided step 2 attached to the testbed, Advanced scanner attached — compared with the mockups; findings fixed or ledgered.
- [ ] **Step 5: Gate** on macOS and Linux container → PASS; `make macos-app`.
- [ ] **Step 6: Commit** `docs+gui: one vocabulary; guide and usage for the new GUI`.
