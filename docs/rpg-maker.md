# RPG Maker mode (JavaScript injection)

Memory scanning works poorly on RPG Maker MV/MZ (and other NW.js/Electron)
games: values live in the V8 JavaScript heap, which the garbage collector
relocates, so a frozen address goes stale and freezing corrupts the heap over
time. The `cheatu-inject` crate takes the approach MTool-class tools use — it
runs code **inside** the game and changes values through the engine's own data
model, which is stable.

This backend is **engine-general**, not per-game: it only uses standard RPG
Maker MV/MZ globals (`$gameParty`, `$gameActors`, `$gameVariables`,
`$gameSwitches`), plus a generic set/freeze/eval/watch mechanism for anything a
specific game stores differently.

## How it works

1. **Detect** — `detect <game-dir>` recognizes RPG Maker MV/MZ / NW.js.
2. **Inject** — `install-agent <game-dir>` drops a small plugin
   (`www/js/plugins/cheatu_agent.js`) and adds a `<script>` tag to
   `index.html` (backed up). Restart the game once to load it.
3. **Bridge** — the plugin and cheatu exchange JSON through a file in the game
   folder (`www/cheatu/`). No network sockets — this works even for Windows
   games under Proton, where Chromium's remote-debugging port does not.
4. **Control** — the plugin applies changes and re-applies frozen values every
   game tick, so freezes are stable and survive the window losing focus.

The agent is identified by a checksum of its own source, so cheatu can tell when
a game is running an older plugin and offer to update it.

## GUI

Switch the header to **RPG Maker (JS)**:

1. **Scan Steam** or **Browse…** to a game folder (loose non-Steam games work
   too; symlinked Steam copies are de-duplicated to the real path).
2. **Install agent**, then launch the game and load a save.

You then get a live panel:

- **Party & resources** — gold with one-click **Set**/**Freeze**, "keep party
  HP/MP/TP maxed" toggles, and the member list (level, HP, MP, TP). These are
  grouped together because they're all party state.
- **Actions** — controls that act *on* the game:
  - **Try recover** is always available; click it whenever the game freezes or
    throws an exception to resume the game loop and clear the error overlay
    without relaunching.
  - Behind an "advanced" disclosure, **Force win / Force lose** end the current
    battle. These use the engine's generic victory/defeat and can destabilize
    games with custom or multi-wave battle systems (e.g. don't force-win the
    final wave), so they're opt-in with a warning.
- **Database** — auto-discovers the game's items, weapons, armors, named
  variables, and switches. Items/weapons/armors have **− / +** step buttons and
  a bulk **+N** (the engine clamps items that cap at 1); variables have an
  editable value; switches are checkboxes. Counts refresh after each change.
- A box to **freeze any custom JavaScript value** for stats the engine stores
  differently.

## CLI

Generic commands (work on any MV/MZ game):

```sh
cheatu-inject install-agent "<game-dir>"      # then restart the game
cheatu-inject agent-party  "<game-dir>"       # gold + every member's HP/MP/TP
cheatu-inject agent-set-gold "<game-dir>" 999999
cheatu-inject agent-freeze-max "<game-dir>" hp all   # party never loses HP (level-safe)
cheatu-inject agent-catalog "<game-dir>"             # discover items/weapons/armors/variables/switches
cheatu-inject agent-give "<game-dir>" item 3 99      # +99 of item 3 (or weapon/armor)
cheatu-inject agent-win "<game-dir>"                 # force the current battle to victory
cheatu-inject agent-lose "<game-dir>"                # force the current battle to defeat
cheatu-inject agent-recover "<game-dir>"             # resume the game loop after a crash
cheatu-inject agent-var "<game-dir>" set 12 100      # $gameVariables[12] = 100
cheatu-inject agent-switch "<game-dir>" 5 on         # $gameSwitches[5] = true
cheatu-inject agent-item "<game-dir>" 3 99           # +99 of item 3
cheatu-inject agent-eval "<game-dir>" '<any JS>'     # escape hatch for custom values
cheatu-inject uninstall-agent "<game-dir>"    # restore index.html, remove plugin
```

Because the escape hatch runs arbitrary JavaScript against the live game, a
game with non-standard stats (for example one that stores a custom "Stamina"
value on top of the standard HP) is handled by pointing
`agent-freeze`/`agent-eval` at the right expression — no code changes needed.

> **Note:** enabling remote debugging via the manifest (`--remote-debugging-port`)
> does **not** work under Wine/Proton — Chromium's DevTools server never starts.
> The file-bridge agent above is the portable approach and is what cheatu uses.
