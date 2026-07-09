# cheatu

A Rust memory scanner for Linux, inspired by [Cheat Engine](https://cheatengine.org/)
and [scanmem](https://github.com/scanmem/scanmem). It lets you attach to a running
process (games in particular), search for values, narrow the results as those
values change, and then edit or freeze them to enable cheats.

It ships as two front-ends over one engine:

- **`cheatu`** — a scanmem-style interactive command line.
- **`cheatu-gui`** — a graphical UI built on [egui/eframe](https://github.com/emilk/egui).
  It is pure Rust (winit + OpenGL), so it runs natively on **both Wayland and
  Xorg** and looks the same under **KDE, GNOME, and standalone window managers**
  without any GTK or Qt dependency.

## Installing

Each GitHub release ships prebuilt packages for the common Linux formats. Pick
whichever suits your distro:

| Format | Install |
|--------|---------|
| **Debian / Ubuntu** (`.deb`) | `sudo apt install ./cheatu_<ver>_amd64.deb` |
| **Fedora / RHEL / openSUSE** (`.rpm`) | `sudo dnf install ./cheatu-<ver>-1.x86_64.rpm` |
| **Arch / CachyOS / Manjaro** (AUR) | `paru -S cheatu` (or `cheatu-git`) |
| **AppImage** (any distro) | `chmod +x cheatu-*.AppImage && ./cheatu-*.AppImage` |
| **Flatpak** | `flatpak install ./cheatu.flatpak` |
| **Snap** | `sudo snap install --classic ./cheatu_*.snap` |

The package installs three binaries — `cheatu` (CLI), `cheatu-gui` (GUI), and
`cheatu-inject` (RPG Maker backend) — plus a desktop entry and icon.

> **Sandbox note:** cheatu's job is to inspect *other* processes' memory, which
> the Flatpak and Snap sandboxes restrict by design. The native `.deb`/`.rpm`,
> the AUR package, and the AppImage give it unrestricted access and are the
> recommended way to run the memory scanner. The Flatpak/Snap builds are handy
> for the RPG Maker (JavaScript-injection) workflow, which only needs the game's
> own files.

The AppImage bundles all three tools: run it as-is for the GUI, or rename/symlink
it to `cheatu` or `cheatu-inject` (or pass `cli` / `inject` as the first argument)
to get the command-line tools.

## Workspace layout

| Crate | Purpose |
|-------|---------|
| `cheatu-core` | The engine: process listing, `/proc/<pid>/maps` parsing, memory read/write, typed scan values, the first/next scan algorithm, and privilege helpers. UI-agnostic. |
| `cheatu-cli`  | The `cheatu` command-line REPL. |
| `cheatu-gui`  | The `cheatu-gui` graphical app. |

## Building

Requires a stable Rust toolchain (1.75+).

```sh
cargo build --release
# binaries: target/release/cheatu  and  target/release/cheatu-gui
```

A `Makefile` wraps the common tasks — `make build`, `make test`, `make lint`,
`make run-gui`, and `make install` (honours `PREFIX`/`DESTDIR`). Run `make help`
for the full list.

## Packaging

Distro packages are built in a container so the toolchain is reproducible and
no packaging tools need to touch your host. Podman is preferred, Docker works
too (auto-detected; override with `CONTAINER_ENGINE=`):

```sh
make packages     # builds .deb, .rpm and .AppImage into ./dist/
make deb          # just the .deb
make rpm          # just the .rpm
make appimage     # just the .AppImage
```

Flatpak and Snap have their own toolchains and build on the host:

```sh
make flatpak      # needs flatpak-builder
make snap         # needs snapcraft
```

The pieces live under `packaging/`:

- `packaging/Containerfile` — the Debian-based build image (Rust + `cargo-deb`,
  `cargo-generate-rpm`, `linuxdeploy`/`appimagetool`).
- `packaging/build.sh` — produces the deb/rpm/AppImage inside that image.
- `packaging/assets/` — the shared desktop entry, AppStream metainfo, and icon
  (app-id `io.github.carroarmato0.cheatu`).
- `packaging/flatpak/` — the Flatpak manifest.
- `packaging/aur/` — `PKGBUILD` (release) and `PKGBUILD-git` for the AUR.
- `snap/snapcraft.yaml` — the Snap definition (classic confinement).

The deb/rpm metadata lives in `crates/cheatu-cli/Cargo.toml` under
`[package.metadata.deb]` / `[package.metadata.generate-rpm]`.

### Continuous integration & releases

`.github/workflows/ci.yml` runs `fmt`, `clippy`, tests, and a release build on
every push and pull request. `.github/workflows/release.yml` fires when a
`v*` tag is pushed: it builds every package format and attaches the artifacts to
the GitHub Release for that tag. (Publishing to the AUR is wired up but optional
— it needs an `AUR_SSH_PRIVATE_KEY` secret.)

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

## CLI usage

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

**Don't know the type?** Use `type any` before the first scan. cheatu then
searches for your value as an i32, u32, i64, u64, f32, and f64 simultaneously,
and each surviving candidate remembers which type actually matched:

```
cheatu> type any
cheatu> scan 100      # matches 100 as int, long, float, double, …
cheatu> next 90       # narrow — each hit is compared using its own type
cheatu> list          # the Type column shows what each address really is
```

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

### RPG Maker MV/MZ and other NW.js games

These are the trickiest targets, so they get their own recipe:

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

## GUI usage

The header has two modes: **Memory scanner** (the default, below) and
**RPG Maker (JS)**. The latter is a point-and-click front-end for the
JavaScript-injection backend: **Scan Steam** or **Browse…** to a game folder
(loose non-Steam games work too), **Install agent**, launch the game, and you
get live gold + party stats with one-click **Set** and **Freeze** (including
"keep party HP/MP/TP maxed") — gold, the keep-maxed toggles, and the member
list are grouped together since they're all party state.

Below the party sits an **Actions** section for things that act *on* the game
rather than editing a value:

- **Try recover** is always available — click it any time the game freezes or
  throws an exception to resume the game loop (and clear the error overlay)
  without relaunching.
- Behind an "advanced" disclosure, **Force win / Force lose** end the current
  battle via the engine's generic victory/defeat. These can destabilize games
  with custom or multi-wave battle systems (e.g. don't force-win the final
  wave), so they're opt-in with a warning.

A **Database** section (auto-loaded, and given the full remaining window height)
discovers the game's items, weapons, armors, named variables, and switches.
Items/weapons/armors have **− / +** step buttons and a bulk **+N** (the engine
clamps items that cap at 1); variables have an editable value; switches are
checkboxes. Counts refresh after each change. There's also a box to freeze any
custom JavaScript value. Same game-agnostic agent as the CLI, no memory scanning
involved.

### Non-Latin text (Japanese / Chinese / Korean)

egui's built-in font is Latin-only, so CJK names (common in RPG Maker games)
would otherwise show as boxes (□□□). On startup cheatu loads a CJK-capable
system font as a fallback — via `fontconfig` (`fc-match`), or a few known font
paths. If you still see boxes, install a CJK font (e.g. `noto-fonts-cjk`) or
point cheatu at a specific font file with the `CHEATU_CJK_FONT` environment
variable.

### Memory scanner

1. **Select process…** and pick your target.
2. Choose a **Value type**, type the current value, and click **First scan**.
   (Tick *Unknown initial value* if you don't know it yet.) If you don't know
   the *type* either — you just see a number on screen — pick
   **Any (unknown type)**; cheatu scans it as every common width at once and the
   results table's **Type** column tells you what each hit really is.
3. Let the value change in-game, pick a **Next scan** comparison
   (equal / greater / less / increased / decreased / changed / unchanged),
   and click **Next scan** to narrow the list.
4. Click **＋** on a result to add it to the **Cheat table** on the right.
5. In the cheat table, edit the value and **Set** it, or tick **Freeze** to keep
   rewriting it continuously.

## RPG Maker / NW.js games — JavaScript injection (stable)

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

How it works:

1. **Detect** — `detect <game-dir>` recognizes RPG Maker MV/MZ / NW.js.
2. **Inject** — `install-agent <game-dir>` drops a small plugin
   (`www/js/plugins/cheatu_agent.js`) and adds a `<script>` tag to
   `index.html` (backed up). Restart the game once to load it.
3. **Bridge** — the plugin and cheatu exchange JSON through a file in the game
   folder (`www/cheatu/`). No network sockets — this works even for Windows
   games under Proton, where Chromium's remote-debugging port does not.
4. **Control** — the plugin applies changes and re-applies frozen values every
   game tick, so freezes are stable and survive the window losing focus.

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

> Note: enabling remote debugging via the manifest (`--remote-debugging-port`)
> does **not** work under Wine/Proton — Chromium's DevTools server never starts.
> The file-bridge agent above is the portable approach and is what cheatu uses.

## Scanning performance

- **Multithreaded:** a first scan splits memory into fixed-size work items and
  distributes them across a pool of worker threads (up to 16, sized to your
  CPU) that share one `/proc/<pid>/mem` handle via thread-safe `pread`. A ~1 GiB
  process scans in well under a second on a typical multi-core machine.
- **Pause target while scanning** (checkbox, on by default): cheatu sends the
  process `SIGSTOP` for the duration of the scan and `SIGCONT` right after, so
  values can't move mid-scan and there's no CPU contention. Turn it off if you
  need the game to keep running during the scan.

## How the engine works

- **First scan** walks the target's readable+writable regions from
  `/proc/<pid>/maps`, reading them in 1 MiB chunks via `/proc/<pid>/mem` and
  testing each type-aligned slot against your value.
- **Next scan** re-reads only the surviving addresses and applies the chosen
  comparison against either your value or the previously observed value
  (for `increased`/`decreased`/`changed`/`unchanged`).
- Floating-point matches use a small tolerance so a displayed `100` still
  matches an in-memory `99.99998`.

## Testing

```sh
cargo test
```

The engine tests scan and patch the test process's *own* memory (always
accessible via `/proc/self/mem`), so they validate the full read → scan →
narrow → write pipeline without needing root.

## Safety & scope

cheatu is intended for use on **your own machine** against software you are
allowed to modify — single-player games, your own programs, debugging, and
learning. Writing into another process's memory can crash it; that's expected
territory for this class of tool.
