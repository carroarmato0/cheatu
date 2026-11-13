# macOS

cheatu runs on macOS (Apple Silicon; Intel should work but is untested) with
the same CLI and GUI as on Linux. There are no prebuilt macOS packages yet, so
build it from source.

## Building

```sh
brew install rustup
rustup default stable
export PATH="$(brew --prefix rustup)/bin:$PATH"   # Homebrew's rustup is keg-only

cargo build --release
# binaries: target/release/{cheatu, cheatu-gui, cheatu-inject}
```

For the GUI, build an app bundle instead of running the bare binary:

```sh
make macos-app        # → dist/cheatu.app (ad-hoc signed)
open dist/cheatu.app  # or drag it to /Applications
```

The bundle carries the cheatu icon, so the Dock shows it for the elevated
instance too; a bare `cheatu-gui` running as root gets the generic "exec" icon.

## Privileges

macOS only hands out another process's memory (its Mach *task port*) to
**root**. cheatu can always inspect itself, but to scan a game:

- **GUI:** when an attach is refused, the main area says *This game needs
  admin rights.* Press **Ask for them**. macOS shows its administrator
  password prompt, and an elevated cheatu replaces the current window and
  re-attaches to the target.
- **CLI:** start it as `sudo cheatu`. Typing `sudo` in the REPL explains this
  instead of relaunching, because the elevated copy would be detached from
  your terminal.

The GUI keeps its settings in `~/Library/Application Support/cheatu/`.

## Hardened runtime and SIP

Even root is refused for processes protected by the **hardened runtime** or
**System Integrity Protection**. Most notarized apps (including many Steam and
App Store games) enable the hardened runtime. When that happens cheatu says
so instead of reporting a plain permission error. Your options:

- Re-sign your copy of the game ad-hoc, without the hardened runtime:
  `codesign --force --deep --sign - /path/to/Game.app`. This only affects
  your local copy; the game's launcher may restore or re-verify it on update.
- Disable SIP (Recovery → `csrutil disable`). This weakens the whole system's
  security; only do it on a machine you're comfortable with that trade-off.

Games running through **Wine, CrossOver or Whisky** are ordinary unsigned
processes and can usually be scanned directly. cheatu marks them `[proton]` in
the process list, just like Proton games on Linux.

## Differences from Linux

| | Linux | macOS |
|---|---|---|
| Memory access | `/proc/<pid>/mem` | `task_for_pid` + Mach VM calls |
| Who may scan | root or `CAP_SYS_PTRACE` | root, and the target must not be hardened |
| Elevation | `pkexec` (polkit) | administrator prompt (GUI) / `sudo` (CLI) |
| `[heap]` / `[stack]` labels | from `/proc/<pid>/maps` | derived from malloc / stack VM tags |
| Settings | `~/.config/cheatu/` | `~/Library/Application Support/cheatu/` |
