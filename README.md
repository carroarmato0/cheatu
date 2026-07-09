<p align="center">
  <img src="packaging/assets/io.github.carroarmato0.cheatu.svg" alt="cheatu" width="128" height="128">
</p>

<h1 align="center">cheatu</h1>

<p align="center">
  <strong>A fast, native memory scanner &amp; cheat tool for Linux.</strong><br>
  Inspired by <a href="https://cheatengine.org/">Cheat Engine</a> and
  <a href="https://github.com/scanmem/scanmem">scanmem</a> — with a modern GUI
  and a stable mode for RPG Maker games.
</p>

<p align="center">
  <a href="https://github.com/carroarmato0/cheatu/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/carroarmato0/cheatu/actions/workflows/ci.yml/badge.svg"></a>
  <a href="https://github.com/carroarmato0/cheatu/releases"><img alt="Release" src="https://img.shields.io/github/v/release/carroarmato0/cheatu?sort=semver"></a>
  <img alt="Platform" src="https://img.shields.io/badge/platform-Linux-informational">
  <img alt="Built with Rust" src="https://img.shields.io/badge/built%20with-Rust-orange?logo=rust">
  <a href="LICENSE"><img alt="License: MIT" src="https://img.shields.io/badge/license-MIT-blue.svg"></a>
</p>

---

cheatu attaches to a running process (games in particular), searches for a
value, narrows the results as that value changes, then lets you **edit or
freeze** it. It ships one engine behind two front-ends — a scanmem-style CLI and
a native GUI — plus a dedicated, stable backend for RPG Maker MV/MZ games.

## ✨ Features

- 🔎 **Fast, multithreaded scanning** — a ~1 GiB process scans in well under a
  second; the target is paused during the scan so values can't move.
- 🖥️ **Native GUI, no GTK/Qt** — pure Rust (egui/eframe on winit + OpenGL), so it
  runs identically on **Wayland and Xorg**, under **KDE, GNOME, and standalone
  window managers**.
- ⌨️ **scanmem-style CLI** — a scriptable REPL for people who live in a terminal.
- ❓ **"Any" type search** — don't know if it's an int, float, or double? Scan
  every common width at once; each hit reports what it really is.
- 🎮 **Steam Play / Proton aware** — the process picker highlights Wine/Proton
  processes, sorts by memory, and labels NW.js/Electron sub-processes by role so
  you attach to the right one.
- 🕹️ **RPG Maker mode (stable)** — instead of fragile heap scanning, cheatu
  injects a game-agnostic JavaScript agent that edits gold, party stats, items,
  variables and switches through the engine's own data model. Works under
  Proton.
- 🈶 **CJK fonts** — Japanese/Chinese/Korean names render properly, no tofu
  boxes.
- 🔐 **One-click elevation** — relaunches itself through `pkexec` (polkit) when
  it needs `CAP_SYS_PTRACE`.
- 📦 **Packaged everywhere** — `.deb`, `.rpm`, AUR, AppImage, Flatpak, and Snap.

## 📦 Installation

Each [release](https://github.com/carroarmato0/cheatu/releases) ships prebuilt
packages. Pick whichever suits your distro:

| Format | Install |
|--------|---------|
| **Debian / Ubuntu** (`.deb`) | `sudo apt install ./cheatu_<ver>_amd64.deb` |
| **Fedora / RHEL / openSUSE** (`.rpm`) | `sudo dnf install ./cheatu-<ver>-1.x86_64.rpm` |
| **Arch / CachyOS / Manjaro** (AUR) | `paru -S cheatu` (or `cheatu-git`) |
| **AppImage** (any distro) | `chmod +x cheatu-*.AppImage && ./cheatu-*.AppImage` |
| **Flatpak** | `flatpak install ./cheatu.flatpak` |
| **Snap** | `sudo snap install --classic ./cheatu_*.snap` |

Every package installs three binaries — `cheatu` (CLI), `cheatu-gui` (GUI), and
`cheatu-inject` (RPG Maker backend) — plus a desktop entry and icon.

> **Which one?** For the memory scanner, prefer the native `.deb`/`.rpm`, the
> AUR package, or the AppImage — inspecting *other* processes needs access that
> the Flatpak/Snap sandboxes restrict by design. The Flatpak/Snap builds are
> handy for the RPG Maker workflow, which only touches the game's own files.

Prefer to build it yourself? See **[Building & packaging](docs/packaging.md)**.

## 🚀 Quick start

### GUI

Launch **cheatu-gui** (from your app menu or the terminal). Pick a target with
**Select process…**, choose a **Value type**, enter the value, and **First
scan**. Change the value in-game, run **Next scan** to narrow, then add a
survivor to the **Cheat table** to **Set** or **Freeze** it.

Playing an RPG Maker MV/MZ game? Switch the header to **RPG Maker (JS)**,
**Browse…** to the game folder, **Install agent**, and get a live panel for
gold, party stats, items, variables and switches — see the
**[RPG Maker guide](docs/rpg-maker.md)**.

### CLI

```console
$ cheatu
cheatu> ps rust           # find your target
cheatu> pid 12345         # attach
cheatu> type i32          # choose a value type
cheatu> scan 100          # first scan for the value 100
cheatu> next < 100        # keep addresses that dropped below 100
cheatu> list              # show survivors with their live values
cheatu> setall 9999       # write to every survivor
```

Don't know the type? `type any` searches every width at once. Full command
reference in the **[usage guide](docs/usage.md)**.

> **Heads-up:** reading another process's memory needs **root** or
> **`CAP_SYS_PTRACE`**. cheatu can elevate itself via `pkexec` — click
> *Request root access* in the GUI, or type `sudo` in the CLI.

## 📖 Documentation

| Guide | What's inside |
|-------|---------------|
| [Usage](docs/usage.md) | Privileges, the CLI REPL, the GUI scanner, finding Proton games, CJK fonts. |
| [RPG Maker mode](docs/rpg-maker.md) | The JavaScript-injection backend for RPG Maker MV/MZ. |
| [Internals](docs/internals.md) | How the scan engine works, performance, and testing. |
| [Building & packaging](docs/packaging.md) | Build from source, the `Makefile`, container packaging, CI/releases. |

## ⚖️ Safety & scope

cheatu is intended for use on **your own machine** against software you are
allowed to modify — single-player games, your own programs, debugging, and
learning. Writing into another process's memory can crash it; that's expected
territory for this class of tool.

## 📄 License

Released under the [MIT License](LICENSE).
