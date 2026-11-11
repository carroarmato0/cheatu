# Usage

- [Privileges](#privileges)
- [CLI](#cli)
- [GUI — memory scanner](#gui--memory-scanner)
- [Cheat tables](#cheat-tables)
- [Address expressions](#address-expressions)
- [Pointer scanner](#pointer-scanner)
- [Cheat wizard](#cheat-wizard)
- [Finding a Steam Play (Proton) game](#finding-a-steam-play-proton-game)
- [Non-Latin text (CJK)](#non-latin-text-japanese--chinese--korean)

For RPG Maker / NW.js games, see the [RPG Maker mode guide](rpg-maker.md).

## Privileges

> On macOS, see the [macOS guide](macos.md) for privileges instead.

Reading and writing another process's memory on Linux requires either running as
**root** or holding **`CAP_SYS_PTRACE`** (and a permissive
`/proc/sys/kernel/yama/ptrace_scope`). cheatu never needs elevation just to
*list* processes — only to attach to one it doesn't own.

Both front-ends can relaunch themselves elevated through **`pkexec`** (polkit),
which shows the standard authentication prompt on KDE, GNOME, and most desktops:

- GUI: click **“Request root access”** in the header.
- CLI: type **`sudo`** at the prompt, or start it with `pkexec cheatu`.

For the GUI, the display environment (`WAYLAND_DISPLAY`, `DISPLAY`,
`XDG_RUNTIME_DIR`, `XAUTHORITY`, …) is forwarded through `pkexec env` so the
elevated instance can still draw to your session.

## CLI

```
$ cheatu
cheatu> ps rust           # find your target
cheatu> pid 12345         # attach
cheatu> type i32          # choose a value type
cheatu> scan 100          # first scan for the value 100
cheatu> next < 100        # keep addresses that dropped below 100
cheatu> next dec          # keep addresses that decreased since last scan
cheatu> list              # show survivors with their live values
cheatu> set 0 9999        # write 9999 to candidate #0
cheatu> setall 9999       # write to every survivor
```

Narrowing operators: a bare number (equals), `= N`, `> N`, `< N`, `!= N`,
`inc`, `dec`, `changed`, `unchanged`. Use `scan ?` for an unknown-initial-value
search.

**Can't read the exact number?** Bracket it: `scan 6400000..6500000` keeps every
value in that range, so a gold counter you only know is "a bit over 6.4M" is
still findable. Then narrow normally as the value changes.

**Don't know the type?** Use `type any` before the first scan. cheatu then
searches for your value as an i32, u32, i64, u64, f32, and f64 simultaneously,
and each surviving candidate remembers which type actually matched:

```
cheatu> type any
cheatu> scan 100      # matches 100 as int, long, float, double, …
cheatu> next 90       # narrow — each hit is compared using its own type
cheatu> list          # the Type column shows what each address really is
```

## GUI — memory scanner

The header has two modes: **Memory scanner** (the default) and
**RPG Maker (JS)** (see the [RPG Maker guide](rpg-maker.md)).

1. **Select process…** and pick your target.
2. Choose a **Value type**, type the current value, and click **First scan**
   (or just press Enter in the value box).
   A range works too — `6400000..6500000` for a number you can only bracket.
   (Tick *Unknown initial value* if you don't know it yet.) If you don't know
   the *type* either — you just see a number on screen — pick
   **Any (unknown type)**; cheatu scans it as every common width at once and the
   results table's **Type** column tells you what each hit really is. *Unknown
   initial value* works with **Any** too — it keeps every address as every type,
   so it needs a lot of memory and aborts with a message if the results wouldn't
   fit; pick a single type if that happens.
3. Let the value change in-game, pick a **Next scan** comparison
   (equal / greater / less / increased / decreased / changed / unchanged),
   and click **Next scan** — or press Enter again — to narrow the list.
   *increased / decreased / changed / unchanged* compare against the last scan
   and ignore the value box.
   A running scan shows its progress in the status bar with a **Cancel**
   button; cancelling leaves the results you already had untouched.
4. Once the whole result set fits on screen, click a column header —
   **Address**, **Type**, **Previous**, **Region** — to sort it. **Region** only
   says what kind of memory an address lives in; it cannot separate two
   candidates in the same region. The 🧪 **probe** can: it writes a test value
   and reports whether the game overwrote it (a display copy) or left it
   (likely the real one). Turn it on under **⚙ Settings → Enable probe** — it
   writes to live game memory. **🧪 Probe N** above the results tests up to 32
   visible addresses in one pass, one at a time, after asking you to confirm;
   frozen rows are skipped, since a frozen value is being rewritten anyway. The
   status bar shows how far it has got, with a **Cancel** button — it stops
   after the address being probed right then, so nothing is left perturbed.
5. Click **+** on a result to add it to the **Cheat table** on the right.
6. In the cheat table, edit the value and **Set** it to write it once, or tick
   **Freeze** to keep rewriting it continuously. Each row shows the address's
   live value on the right, so you can see the write land. **🗑 Clear all**
   empties the table in one go.

Not sure which address is the real one, or want the cheat to keep working after
the game restarts? **New cheat…** in the header opens the
[cheat wizard](#cheat-wizard), which does all of the above step by step.

## Cheat tables

The cheat table can be saved to a file and opened again later, or shared with
someone else.

- **💾 Save**, **Save as…** and **📂 Open…** sit above the table. The file is
  plain TOML (`.toml`), readable and editable by hand:

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

  `type` is `i8` … `u64`, `f32`, `f64`, or `bytes` (then `value` is hex bytes
  and fixes the length). A file only holds what you see: description,
  address expression, type, value and freeze flag — never resolved addresses or
  process ids.
- Each row's address is an [address expression](#address-expressions) that is
  re-resolved against the running game, so a table keeps working across
  restarts when its expressions are stable. Next to it you see the resolved
  address, or the error in red. A row that does not resolve stays in the table,
  is never written to, and is retried every few seconds.
- Adding a scan result with **+** makes the row `"module"+offset` when the
  address lies in a loaded image (the executable or a library), otherwise a
  plain address.
- The **…** menu of a row offers **Make AoB signature** (a byte pattern around
  the address that survives restarts; best for code and static data) and
  **Pointer scan…** (see [Pointer scanner](#pointer-scanner); the right tool
  for values on the heap).
- **Opening a table never writes to the game's memory.** All freezes start
  off, whatever the file says; rows that were frozen when saved are marked
  *frozen when saved* until you tick ❄ yourself. This makes it safe to open a
  table someone else gave you: nothing happens until you freeze or **Set** a
  row.
- When the table names a `process` and nothing is attached, cheatu offers to
  attach to the running process with that name (with several matches, the
  process picker opens filtered on it). Attached to a process with another
  name, you get a warning but the table still opens.
- A table that fails to load (invalid TOML, unknown type, a value that does
  not fit its type, an unknown `version`, a file over 1 MiB or with more than
  10 000 entries) is refused as a whole, with the line number in the status
  bar; the current table is left untouched. Saving writes a temporary file and
  renames it, so a failed save never damages an existing table.

The CLI has the same commands: `save <file>`, `load <file>` (freezes stay off),
`add <expr> [type] [description…]`, `entries` and `resolve <expr>`.

## Address expressions

An address expression names an address the same way in the GUI, the CLI and
table files, and is resolved again against the running process each time:

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

- **Hex needs `0x`**: `10` is always ten, `0x10` is sixteen.
- `"name"` is the base (lowest address) of the loaded image whose file name is
  exactly `name` (case-sensitive). No match is an error, and so are two
  different images with the same file name.
- `[x]` reads 8 bytes at `x` (little-endian); cheatu supports 64-bit targets
  only.
- `aob(pattern)` and `aob(pattern, "name")` search readable memory (of that
  module only, when given) for a byte pattern with `??` wildcards, as in an AoB
  scan. It must match exactly once; zero or several matches is an error that
  says how many.
- `+` and `-` wrap around at 64 bits; `-` is there for negative offsets.
- Whitespace between tokens is allowed.
- Limits: at most **1024 characters** and **64 levels** of `[…]` / `(…)`
  nesting.

Parse and resolve errors say where and what failed (`module "dmhost" not
found`, `pointer at 0x… unreadable`, `aob(…) matched 3 times, must be unique`),
in red under the field in the GUI and as a message in the CLI.

## Pointer scanner

Values on the heap move every time the game starts, so neither a plain address
nor `"module"+offset` survives a restart. The pointer scanner finds *pointer
chains* from the game's static memory (inside a module image) to the address,
such as `[["dmhost"+0x1a2b30]+0x48]+0x6337e134`, which the game itself follows
to reach the value.

Open it from a cheat table row: **…** → **Pointer scan…**.

- **Settings** (defaults in brackets): **Depth** — number of `[…]` steps, 1 to
  8 [5]; **Max offset** per step, 0x8 to 0x100000 [0x1000]; **Alignment** of
  pointers, 1, 2, 4 or 8 [8]; **Result cap**, 1 to 10 000 000 [1 000 000].
- **Scan** first takes a snapshot of every pointer in the game's writable
  memory (the *pointer map*, stored and sorted in a temporary file on disk, so
  memory use stays bounded), then searches it backwards from the target. With
  **Pause target while scanning** on, the game is paused only while its memory
  is read. Progress is shown, and **Cancel** stops it. For a game using 5 GiB
  of memory, reading took about 14 s and the map needs about 0.6 GiB of
  temporary disk space.
- Chains are listed shortest first. **Add to table** puts the selected chain
  in the cheat table.

**Narrowing the chains across restarts.** A first scan usually finds many
chains, and most of them only work by accident. To keep the robust ones:

1. Run the scan and **Save results…** (a `.cheatu-ptr` file).
2. Restart the game and attach cheatu to the new process.
3. Find the value again (a normal value scan, or the wizard), then **Filter
   with new address** with its new address — or, without the address,
   **Filter by value** with the value it holds now. Only chains that still lead
   there are kept. Filtering needs only the chains and the running game, not a
   new pointer map.
4. Repeat steps 2–3 until a few chains remain, then add one to the table.

**Results files are treated as untrusted**, since they are meant to be shared:
**Open results…** checks every length and index and refuses a malformed file.
The settings stored in a file are only shown, never used for a new scan.

CLI: `ptrscan <expr|hex> [depth] [max-offset]`, `ptrscan filter addr
<expr|hex>`, `ptrscan filter value <value>`, `ptrscan save <file>`,
`ptrscan load <file>` and `ptrscan list [n]`.

## Cheat wizard

**New cheat…** in the header (available once a process is attached) opens a
wizard that walks you through the whole flow without needing to know how
memory scanners work. It uses its own scan, so the main results are left
alone. Every long step shows what it is doing, with **Cancel** where it can
stop; **Start over** is always available.

1. **Find the value.** Pick the kind of number (4 Bytes for most counters) and
   enter the value the game shows now. Then change it in the game and enter the
   new value, until at most 10 addresses are left. For a value without a number
   on screen (a health bar), use **Increased** / **Decreased** / **Unchanged**
   instead.
2. **Find the real one.** With your consent, cheatu writes test values: it
   first drops the copies the game overwrites at once, then adds 1 to each
   remaining address in turn and asks whether the game shows the change. The
   one you confirm keeps your value + 1; the others are put back. Without
   consent you pick an address from the list yourself.
3. **Make it last.** An address inside a module image is final
   (`"module"+offset`). Otherwise the wizard runs the
   [pointer scanner](#pointer-scanner) and asks you to restart the game. cheatu
   stays open, checks every 2 seconds for a new process with the same name,
   attaches to it (the main window follows, so the cheat table re-resolves),
   and asks for the value again to filter the chains. This repeats until at
   most 3 chains are left, or you stop and keep the best one.
4. **Save.** Give the cheat a name and a value, and choose whether it stays
   frozen at that value or is only saved (use **Set** later). It is added to the
   cheat table, which you can then save and share.

When the value cannot be found, no candidate survives, or no chain survives a
restart, the wizard says so and offers to start over or — once an address was
chosen — to keep the plain address as a cheat **for this session only**. That
address may not be the real one and does not survive a restart; such cheats
are marked *(this session only)* in the table.

## Finding a Steam Play (Proton) game

Proton runs a whole swarm of processes (`steam.exe`, `services.exe`,
`explorer.exe`, `winedevice.exe`, pressure-vessel/reaper wrappers, …) alongside
the game, so the picker helps you spot the real one:

- Every Wine/Proton process is tagged **proton** and highlighted; tick
  **“Wine/Proton only”** to hide everything else.
- Click the **Memory** column to sort by resident memory — a game almost always
  dominates. When you open the picker, cheatu pre-selects the heaviest
  Wine/Proton process for you.
- For engines that spawn multiple processes (NW.js / RPG Maker MV/MZ, Electron,
  Chromium-based games), each subprocess is labelled with a **Role** column
  (`main`, `renderer`, `gpu-process`, `utility`, …). The game's data lives in
  the largest **renderer** — cheatu highlights it in green and pre-selects it.

### RPG Maker MV/MZ and other NW.js games (memory scanning)

These are the trickiest targets. For a *stable* experience prefer the
[JavaScript-injection backend](rpg-maker.md); if you still want to scan memory:

1. **Process:** open the picker, tick *Wine/Proton only*, sort by *Memory*.
   You'll see several `nw.exe` entries with different roles. **Attach to the
   biggest one whose Role is `renderer`** — not `main` and not `gpu-process`
   (the GPU process is often large but holds no game state). cheatu
   pre-selects this for you.
2. **Value type:** the game stores numbers in a JavaScript (V8) heap. Scan
   with type **`f64` (double)** — that's the representation these tools reliably
   find for RPG Maker MV/MZ. If a double search comes up empty, try **`f32`**,
   then **Any**. A plain 4-byte integer search usually will *not* find them,
   because V8 doesn't store JS numbers as raw `int32`.
3. **Narrow by change, not by exact value.** V8 moves and boxes numbers, so the
   surest workflow is: first scan the current amount as a double → do something
   in-game that changes it (spend/earn gold, take damage) → *Next scan* with
   the new amount → repeat until a handful of addresses remain. `increased` /
   `decreased` also work well when you don't want to read the exact number.

## Non-Latin text (Japanese / Chinese / Korean)

egui's built-in font is Latin-only, so CJK names (common in RPG Maker games)
would otherwise show as boxes (□□□). On startup cheatu loads a CJK-capable
system font as a fallback — via `fontconfig` (`fc-match`), or a few known font
paths. If you still see boxes, install a CJK font (e.g. `noto-fonts-cjk`) or
point cheatu at a specific font file with the `CHEATU_CJK_FONT` environment
variable.
