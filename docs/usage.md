# Usage

- [Privileges](#privileges)
- [CLI](#cli)
- [GUI — memory scanner](#gui--memory-scanner)
- [Finding a Steam Play (Proton) game](#finding-a-steam-play-proton-game)
- [Non-Latin text (CJK)](#non-latin-text-japanese--chinese--korean)

For RPG Maker / NW.js games, see the [RPG Maker mode guide](rpg-maker.md).

## Privileges

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
   frozen rows are skipped, since a frozen value is being rewritten anyway.
5. Click **+** on a result to add it to the **Cheat table** on the right.
6. In the cheat table, edit the value and **Set** it to write it once, or tick
   **Freeze** to keep rewriting it continuously. Each row shows the address's
   live value on the right, so you can see the write land. **🗑 Clear all**
   empties the table in one go.

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
