# The testbed

`cheatu-testbed` is a tiny text dungeon game made to be cheated. Its values sit
in memory on purpose, in every kind of place cheatu has to deal with: statics
in the program image, values behind a pointer chain on the heap, a decoy copy,
and a byte signature. Use it to learn cheatu or to try a change, without
risking a real game. The same program is the target of cheatu's integration
tests.

It is not packaged; build it from the source tree.

## Running it

```sh
make testbed
```

This builds it in release mode and starts it. On macOS it is also signed with
`get-task-allow` (see `crates/cheatu-testbed/entitlements.plist`), so cheatu
can attach to it **without root** — no administrator prompt needed. On Linux,
cheatu needs the usual privileges (see [Usage](usage.md)), or a Yama
`ptrace_scope` of 0.

```
cheatu-testbed — pid 75314. Type help.
Level 1  HP 100/100  Mana 50  XP 0  Gold 100  Ammo 10  Kills 0
> fight
Level 1  HP 93/100  Mana 50  XP 12.5  Gold 115  Ammo 10  Kills 1
```

| Command | Effect |
|---|---|
| `status` | show the values |
| `fight` | HP −7, XP +12.5, Kills +1, Gold +15 |
| `heal` | Mana −10, HP +20 (max 100); needs 10 mana |
| `shoot` | Ammo −1; needs ammo |
| `buy` | Gold −20, Ammo +5; needs 20 gold |
| `next` | next level: HP, mana, XP and ammo **move to new memory**; Mana +25 |
| `addrs` | print where every value really is (the answers) |
| `get <name>` | print one value |
| `marker` | print the 16-byte marker, as a ready-to-paste `aob("…")` |
| `help`, `quit` | |

At 0 HP you die and everything starts over, including the memory layout.
Nothing changes on its own: values only change when you type a command.

## What is where

| Value | Type | Where | Good for practising |
|---|---|---|---|
| Gold | u32 (4 bytes) | static, in the program image | exact scans, `"cheatu-testbed"+0x…` addresses |
| Kills | u64 (8 bytes) | static, zero-initialised (`.bss`) | static addresses in `.bss` |
| Gold (shown) | u32 | static, copied from Gold before every command | the decoy: two results that change together |
| HP | i32 (4 bytes) | heap: `WORLD → Level → Player` | freezing, pointer scans |
| Mana | f32 (float) | heap, in `Player` | float scans |
| XP | f64 (double) | heap, in `Player` | double scans |
| Ammo | i16 (2 bytes) | heap, between fixed bytes | AoB signatures |
| Marker | 16 bytes `CHEATU-TESTBED!!` | start of `Player` | `aob(…)` addresses, byte searches |

`addrs` tells you the true addresses, so you can always check what cheatu
found.

## Exercises

Start the testbed in one terminal and cheatu-gui next to it, and attach to
`cheatu-testbed` from the start screen. Exercises 1 to 5 need **Advanced**
ticked in the header; exercise 6 uses it off.

### 1. Find the gold, and the decoy

1. Search for `100` as **4 Bytes** (**🔍 Search**).
2. `fight` in the testbed (gold becomes 115), then **Narrow down** for `115`.
3. A couple of rounds of `buy`/`fight` leave **two** results. `addrs` shows
   they are `gold` and `gold_shown`.
4. Add both with **+**, set the second one to 999 in the cheat table and type
   `status`: it is back to the real gold. Set the first one: that change
   sticks. The real value is the one the game
   computes with; a display copy is overwritten from it.

### 2. Freeze HP

Find HP (100 → `fight` → 93), add it to the cheat table with **+** and freeze it (**❄**). `fight`
as often as you like: HP stays put. Now type `next` and `fight` again: the
frozen row no longer helps, because HP moved to new memory (the old address
stays allocated but unused, so the freeze is harmless). Exercise 4 fixes
that.

### 3. Survive a restart with a table

Add Gold and Kills to the cheat table — both static, so cheatu stores them as
`"cheatu-testbed"+0x…`. Save the table (💾 Save), `quit` the testbed, start it
again, attach, and open the table (📂 Open…). Both rows find their values in
the new process.

### 4. Pointer-scan HP

1. With HP in the table, click **🎯 Pointer scan…** on its row, then **Scan**.
2. Quit and restart the testbed, attach, and filter the chains by HP's new
   address with **Filter with new address** (from `addrs`, or found with a
   quick scan). One or more chains
   survive.
3. Type `next`: the surviving chains still point at HP. Add one to
   the table and freeze it; it now holds across levels and restarts.

### 5. Signatures

On the Ammo row click **🧬 AoB signature**. cheatu finds a unique byte
pattern around it and stores `aob("…")`; it survives restarts and `next`.
For the marker, type `marker` in the testbed: it prints a ready-made
`aob("43 48 …")` address. Add it to the table as an address, or search the
bytes between the quotes as an Array of Bytes.

### 6. The guided steps

With **Advanced** off, attach and follow the steps for Gold: cheatu scans,
narrows down to the real value (spotting the decoy), and offers to make it
last across restarts before you **Add to My cheats**.

## In the tests

`cargo test -p cheatu-testbed` starts the testbed with `--script`, a line
protocol (`READY pid=…`, then one `OK …` or `ERR …` reply per command) with
the same commands. The tests attach cheatu-core and check every result
against `addrs`: scans, static addresses, pointer chains, `aob(…)`,
signatures, and a saved table, each across a restart. See
[Internals](internals.md#testing).
