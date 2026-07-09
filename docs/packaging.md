# Building & packaging

## Building from source

Requires a stable Rust toolchain (1.75+).

```sh
cargo build --release
# binaries: target/release/{cheatu, cheatu-gui, cheatu-inject}
```

A `Makefile` wraps the common tasks:

```sh
make build        # release build of the whole workspace
make test         # run the test suite
make lint         # rustfmt --check + clippy
make run-gui      # build & run the GUI
make run-cli      # build & run the CLI
make install      # install under $(PREFIX) (honours PREFIX / DESTDIR)
make help         # the full list
```

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
- `packaging/aur/` — `PKGBUILD` (release) and `PKGBUILD-git`, plus `.SRCINFO`.
- `snap/snapcraft.yaml` — the Snap definition (classic confinement).

The deb/rpm metadata lives in `crates/cheatu-cli/Cargo.toml` under
`[package.metadata.deb]` / `[package.metadata.generate-rpm]`. One package named
`cheatu` ships all three binaries plus the desktop integration.

### Sandbox caveat (Flatpak / Snap)

cheatu inspects and edits *other* processes' memory, which the Flatpak and Snap
sandboxes restrict by design. The native `.deb`/`.rpm`, the AUR package, and the
AppImage give it unrestricted access and are the recommended way to run the
memory scanner. The Flatpak manifest opens the sandbox as far as is reasonable
(`--allow=devel` for ptrace, `--filesystem=host`) and the Snap uses classic
confinement, but pkexec-based elevation does not cross the sandbox cleanly.
Those builds are aimed mainly at the RPG Maker workflow, which only needs the
game's own files.

## Continuous integration & releases

- `.github/workflows/ci.yml` runs `fmt`, `clippy`, tests, and a release build
  on every push and pull request.
- `.github/workflows/release.yml` fires when a `v*` tag is pushed: it builds
  every package format and attaches the artifacts to the GitHub Release for that
  tag.

Cutting a release:

```sh
git tag v0.1.0
git push origin v0.1.0
```

Publishing to the AUR is wired up but optional — it runs only on the canonical
repository and needs an `AUR_SSH_PRIVATE_KEY` secret. For a Flathub submission,
replace the manifest's network build with an offline `cargo-sources.json`
(`make flatpak-sources`).
