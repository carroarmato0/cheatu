# GUI revamp: guided by default, advanced on demand

Date: 13/11/2026 · Branch: `feat/gui-revamp` (from `main` at 63b75de)

## Goal

Make cheatu-gui serve beginners and experienced users equally (choice C in
the evaluation). A beginner sees one obvious next step at every moment and
reaches a working cheat without jargon. An experienced user flips one
**Advanced** switch and gets the full scanner and every tool in view.

Evaluation that led here: scores ★★ ease of use, ★★★★ usefulness, ★★★
intuitiveness, ★★ cleanness. Top problems: no clear first step and a
hard-to-find wizard; crowded cheat-table rows showing address expressions to
everyone; no safety nets (unsaved changes, deletes); feedback only in the
status bar or hovers; inconsistent words for the same things.

Mockups (approved): `.superpowers/brainstorm/93666-1794584084/content/`
`start-screen-v2.html`, `attached.html` (option A + advanced layout),
`my-cheats.html`.

## Screens and flow

```
no game ──Attach──▶ RPG Maker game? ──yes──▶ choice: RPG Maker mode | Memory scanner
   ▲                    │ no                         │                   │
   │                    ▼                            ▼                   ▼
   └──game ▾ menu── guided steps (Advanced off) ◀── or ──▶ scanner (Advanced on)
                    "My cheats" panel on the right in both
```

### 1. Header

Left: `cheatu`, then, once attached, the game's name with a ▾ menu:
**Change game…**, **Detach**, and for RPG Maker games **RPG Maker mode** /
**Memory scanner** (switch). Right: **✨ Guided…** (only with Advanced on),
an **Advanced** switch, ⚙, ⓘ.

Removed from the header: the "Memory scanner" / "RPG Maker (JS)" tabs,
"Select process…", "New cheat…", "Request root access", "🔒 Elevate &
attach", the "limited privileges" / "root" label.

The Advanced switch is saved in `config.json` (`"advanced": bool`, default
false) next to `"theme"`, and so is `"probe": bool` (today "Enable probe" is
not saved at all). Unknown or missing keys keep their defaults; existing
files with only `"theme"` stay valid.

### 2. Start screen (no game attached)

The process list *is* the start screen (no modal):

- Step bar: **1 Choose your game** · 2 Find a value · 3 Keep it.
- Heading "Which game do you want to cheat in?", line "Start the game
  first. The most likely one is already selected."
- Filter box, list (Program, Memory), best guess preselected (today's
  `pick_best_process`). Double-click attaches.
- Tags per row: **Wine** for Wine/Proton, **RPG Maker MV/MZ** for a detected
  RPG Maker game, 🔒 "needs admin rights" when not accessible.
- **Multi-process apps fold into one row**: NW.js/Chromium helper processes
  (role gpu-process, utility, crashpad…) of the same program are hidden; the
  row stands for the program and Attach picks its largest renderer (today's
  rule). Advanced on shows every process with PID and Role, as today.
- Buttons: **Attach** (primary), **📂 Open a saved cheat table…**; check box
  "Wine/Proton only". Link **"RPG Maker game not running yet? Set it up…"**
  opens the RPG Maker screen without a game.
- The "renderer / NW.js" tip line is removed.
- Admin rights: Attach on a 🔒 row (or a refused attach) shows inline:
  "This game needs admin rights. **Ask for them**" — which runs today's
  elevate-and-reattach. No separate header button.

The process-picker window stays only for the "table made for X, several X
running" case, reusing the same list widget.

### 3. RPG Maker detection and choice

- From a process: take the program path (argv[0] of the cmdline; under
  Wine translate `Z:\…` to `/…` and other drive letters through the Wine
  prefix's `dosdevices` when `WINEPREFIX` is in its environ, else skip),
  walk up from its directory at most 3 levels and run
  `cheatu_inject::detect_dir` on each; the first `is_rpgmaker()` hit wins.
  Detection runs when the list refreshes, cached per pid, off the UI thread
  if it takes longer than a frame (file checks only).
- Attaching to a detected game shows a choice screen: heading "{name} is an
  RPG Maker {MV|MZ} game", two cards — **RPG Maker mode** (recommended;
  "Gold, items, party HP/MP, switches · through the game's own code: nothing
  corrupts · one-time setup, then restart the game") and **Memory scanner**
  ("Any number you can see · works right now, no restart · cheats may break
  after an update"). The choice can be switched from the game ▾ menu.
- RPG Maker mode is today's RPG Maker tab content, with the game folder
  prefilled from detection. Its inside is not redesigned in this round.

### 4. Guided steps (Advanced off)

The cheat wizard moves from its floating "New cheat" window into the main
area, under the step bar (2 Find a value, 3 Keep it). Its state machine
(`wizard.rs`) is unchanged; `wizard_ui.rs` renders into a panel instead of a
window. Wording follows the vocabulary below; the type list shows friendly
names only ("4 Bytes", not "4 Bytes (i32)").

- Step 2 starts with "What value do you want to change?" + value + **Search**,
  link "Search for an unknown value" (bars, gauges), link **Scan manually**
  (turns Advanced on).
- After step 4 (save to My cheats) the guide offers **Find another value**,
  which starts again at step 2.
- **Start over** remains on every step after the first.

### 5. Scanner (Advanced on)

Today's scanner with these changes:

- First/Next scan become **Search** / **Narrow down**; Next-scan controls
  and Undo are shown only after a first search.
- A disabled button shows its reason as small text under the controls, not
  only on hover.
- Results: Region column and 🧪 probe as today; probe is available without
  visiting Settings when Advanced is on (Settings keeps the switch for
  the guided flow).
- "+" on a result that is already in My cheats shows "Already in My cheats".

### 6. My cheats panel

Title **My cheats** (Advanced: **Cheat table**), the file name, and a **•**
when there are unsaved changes. Buttons: 📂 Open, 💾 Save (Advanced also
**Save as…**), ⋯ (Save as…, Clear all…). Empty state: "Nothing yet. Cheats
you find land here." The panel's default width shrinks to fit (min 260 px)
until it has rows.

Basic row (one line): **❄ Freeze** labelled toggle, name, value field,
**Set**, a status mark (**⚠ this session only** for a plain address that
won't survive a restart), ▸/▾ to unfold. Details: "✓ Keeps working after a
restart" or the ⚠ explanation with **Make it last…** (pointer scan for heap
addresses, AoB signature for static/module ones — the same choice the
wizard makes), "Now {v} in the game · {type}", **Rename**, **Remove…**.

Advanced row: ❄ toggle, name, type, value, Set, ⋯; details always open:
editable expression with "→ address = value", buttons **🎯 Pointer scan…**,
**🧬 AoB signature**, **⟳ Re-resolve**, **🗑**. Type can be changed from ⋯.

"Survives a restart" = the expression contains a dereference or `aob(…)`
or is static (`AddrExpr::is_static`); otherwise it is "this session only".

### 7. Feedback

- A short confirmation appears next to the control that caused it for
  ~3 s ("✓ Added Rupees to My cheats", "✓ Wrote 999 to Rupees", "Already in
  My cheats").
- Errors appear in the same place and stay until dismissed (×).
- The status bar keeps progress (scan, probe, pointer scan) with Cancel, and
  the last message.

### 8. Safety nets

- Unsaved changes = My cheats differs from the last save or open (rows,
  names, values, expressions, freeze flags); a new empty table is not dirty.
- Closing the window, Open, Change game with a table from another game, and
  Clear all ask: "Save changes to {file}?" **Save** / **Don't save** /
  **Cancel** (for a never-saved table: "Save your cheats?").
- **Remove…** and **Clear all…** ask for confirmation.

## Vocabulary

| Concept | Word | Replaces |
|---|---|---|
| running program | **game** (list: "program") | process, pid, target |
| saved values | **My cheats** (Advanced: **Cheat table**) | table, saved addresses, rows, entries |
| one entry | **cheat** | row, entry, address |
| keep fixed | **Freeze** | ❄ / tick |
| survives restart | **keeps working after a restart** / **this session only** | pointer chain, plain address (in basic view) |
| probe | **Test** (Advanced: **🧪 Probe**) | test values, test writes |
| admin | **needs admin rights**, asked at Attach | Request root access, Elevate & attach, limited privileges |
| scanning | **Search**, **Narrow down** | First scan, Next scan |

Technical words (address, expression, pointer chain, AoB) remain in Advanced
view and in `docs/usage.md`.

## Small fixes

- About: "A memory scanner and cheat tool for games on Linux and macOS,
  including Wine/Proton and RPG Maker titles."
- "Enable probe" and Advanced are saved (above).
- The cheat-table hint with expression syntax is removed from basic view.

## Out of scope (follow-ups)

- Redesigning the inside of the RPG Maker screen (20+ controls, jargon).
- Merging the two folder browsers (file dialog and game-folder browser).
- The pointer-scan window's contents (it is opened from the new buttons).
- Keyboard shortcuts, localisation.

## Architecture notes

- `main.rs` is ~3800 lines. Split by screen as part of this work:
  `start_screen.rs` (game list, detection tags, admin-rights inline),
  `my_cheats.rs` (panel, rows, dirty tracking, confirmations),
  `feedback.rs` (inline confirmations/errors), `settings.rs` (config.json
  load/save for theme, advanced, probe), `rpg_detect.rs` (process → game
  folder → engine). The scanner and RPG Maker code stay in `main.rs` for
  now unless a split falls out naturally.
- A `Screen` value derived each frame from state (no game / RPG choice /
  guided / scanner / RPG Maker mode) decides what the central panel draws;
  it is a pure function of state, so it is unit-testable.
- Dirty tracking compares a snapshot of the saved `CheatTable` with the
  current one (`CheatTable: PartialEq` exists).

## Testing

- Unit tests: `Screen` selection for each state; config load/save with
  missing/unknown keys and the old theme-only file; dirty tracking (edit,
  save, open, undo an edit back to saved = clean); "survives a restart"
  classification; vocabulary — no user-visible string in `cheatu-gui`
  contains "pid", "process" (except the advanced process list), "First scan",
  "Next scan", "entries", "saved addresses" (a grep test over the source's
  string literals with an allowlist); RPG Maker detection from paths incl.
  `Z:\` Wine paths, using temp dirs with `js/rmmz_core.js` / `js/rpg_core.js`.
- Screenshots of each screen against the testbed with `--attach` (no
  clicks), compared with the mockups.
- Manual test by the user of the clicking flows (guided steps, confirmations,
  Advanced switch, RPG Maker choice with a real MV/MZ game if available).

## Risks

- Moving the wizard into the main area touches the most UI code; its state
  machine is untouched and already unit-tested, which limits the risk.
- Folding multi-process apps can hide the process a user wants; Advanced
  shows every process.
- Wine path translation without `WINEPREFIX` falls back to no detection
  (the scanner still works).
