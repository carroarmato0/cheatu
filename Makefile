# cheatu — developer and packaging targets.
#
# Development:   make build | test | lint | run-gui | run-cli
# Install:       sudo make install            (PREFIX=/usr/local by default)
# Packaging:     make packages                (deb + rpm + AppImage, in a container)
#                make deb | rpm | appimage    (one format, in a container)
#                make flatpak | snap          (host tools: flatpak-builder / snapcraft)
#                make aur-srcinfo             (regenerate packaging/aur/.SRCINFO)
#
# Run `make help` for the annotated list.

SHELL       := /bin/bash
PREFIX      ?= /usr/local
DESTDIR     ?=
APP_ID      := io.github.carroarmato0.cheatu
VERSION     := $(shell grep -m1 '^version' Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')
BINS        := cheatu cheatu-gui cheatu-inject

# Container engine: prefer podman, fall back to docker.
CONTAINER_ENGINE ?= $(shell command -v podman 2>/dev/null || command -v docker 2>/dev/null)
IMAGE       ?= cheatu-build
# SELinux-friendly bind mount; override with MOUNT_OPTS= on systems that choke.
MOUNT_OPTS  ?= :Z
RUN_IN_CTR   = $(CONTAINER_ENGINE) run --rm \
	-v "$(CURDIR)":/work$(MOUNT_OPTS) \
	-v cheatu-cargo-registry:/usr/local/cargo/registry \
	-w /work $(IMAGE)

.DEFAULT_GOAL := help

# ---------------------------------------------------------------------------
# Development
# ---------------------------------------------------------------------------

.PHONY: build
build: ## Build the whole workspace in release mode
	cargo build --release --locked

.PHONY: debug
debug: ## Build the whole workspace in debug mode
	cargo build --locked

.PHONY: test
test: ## Run the test suite
	cargo test --workspace --locked

.PHONY: fmt
fmt: ## Format the code
	cargo fmt --all

.PHONY: fmt-check
fmt-check: ## Check formatting (CI)
	cargo fmt --all --check

.PHONY: clippy
clippy: ## Lint with clippy, warnings as errors
	cargo clippy --workspace --all-targets --locked -- -D warnings

.PHONY: lint
lint: fmt-check clippy ## fmt-check + clippy

.PHONY: run-gui
run-gui: ## Build and run the GUI
	cargo run --release -p cheatu-gui

.PHONY: run-cli
run-cli: ## Build and run the CLI
	cargo run --release -p cheatu-cli

.PHONY: clean
clean: ## Remove build artifacts and packaged output
	cargo clean
	rm -rf dist target/AppDir target/debian target/generate-rpm

# ---------------------------------------------------------------------------
# Install / uninstall (used by the AUR PKGBUILD and for local installs)
# ---------------------------------------------------------------------------

.PHONY: install
install: build ## Install binaries + desktop integration under $(PREFIX)
	install -d "$(DESTDIR)$(PREFIX)/bin"
	for b in $(BINS); do \
		install -Dm755 "target/release/$$b" "$(DESTDIR)$(PREFIX)/bin/$$b"; \
	done
	install -Dm644 packaging/assets/$(APP_ID).desktop \
		"$(DESTDIR)$(PREFIX)/share/applications/$(APP_ID).desktop"
	install -Dm644 packaging/assets/$(APP_ID).metainfo.xml \
		"$(DESTDIR)$(PREFIX)/share/metainfo/$(APP_ID).metainfo.xml"
	install -Dm644 packaging/assets/$(APP_ID).svg \
		"$(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps/$(APP_ID).svg"
	install -Dm644 LICENSE "$(DESTDIR)$(PREFIX)/share/licenses/cheatu/LICENSE"

.PHONY: uninstall
uninstall: ## Remove an installation done by `make install`
	for b in $(BINS); do rm -f "$(DESTDIR)$(PREFIX)/bin/$$b"; done
	rm -f "$(DESTDIR)$(PREFIX)/share/applications/$(APP_ID).desktop"
	rm -f "$(DESTDIR)$(PREFIX)/share/metainfo/$(APP_ID).metainfo.xml"
	rm -f "$(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps/$(APP_ID).svg"
	rm -f "$(DESTDIR)$(PREFIX)/share/licenses/cheatu/LICENSE"

# ---------------------------------------------------------------------------
# Container-based packaging (deb / rpm / AppImage)
# ---------------------------------------------------------------------------

.PHONY: require-engine
require-engine:
	@test -n "$(CONTAINER_ENGINE)" || \
		{ echo "error: no container engine found (install podman or docker)"; exit 1; }

.PHONY: container-image
container-image: require-engine ## Build the packaging container image
	$(CONTAINER_ENGINE) build -t $(IMAGE) -f packaging/Containerfile .

.PHONY: packages
packages: container-image ## Build deb + rpm + AppImage in the container
	$(RUN_IN_CTR) packaging/build.sh all

.PHONY: deb
deb: container-image ## Build the .deb in the container
	$(RUN_IN_CTR) packaging/build.sh deb

.PHONY: rpm
rpm: container-image ## Build the .rpm in the container
	$(RUN_IN_CTR) packaging/build.sh rpm

.PHONY: appimage
appimage: container-image ## Build the .AppImage in the container
	$(RUN_IN_CTR) packaging/build.sh appimage

# ---------------------------------------------------------------------------
# Flatpak / Snap (their own toolchains; run on the host)
# ---------------------------------------------------------------------------

.PHONY: flatpak
flatpak: ## Build & install the Flatpak (needs flatpak-builder)
	flatpak-builder --user --install --force-clean \
		target/flatpak-build packaging/flatpak/$(APP_ID).yaml

.PHONY: flatpak-sources
flatpak-sources: ## Regenerate offline cargo-sources.json for Flathub
	@command -v flatpak-cargo-generator >/dev/null 2>&1 || { \
		echo "Get flatpak-cargo-generator from"; \
		echo "  https://github.com/flatpak/flatpak-builder-tools/tree/master/cargo"; \
		exit 1; }
	flatpak-cargo-generator Cargo.lock -o packaging/flatpak/cargo-sources.json

.PHONY: snap
snap: ## Build the snap (needs snapcraft)
	snapcraft pack --output dist/cheatu_$(VERSION)_amd64.snap

# ---------------------------------------------------------------------------
# AUR
# ---------------------------------------------------------------------------

.PHONY: aur-srcinfo
aur-srcinfo: ## Regenerate packaging/aur/.SRCINFO (needs makepkg)
	cd packaging/aur && makepkg --printsrcinfo > .SRCINFO

.PHONY: check-release
check-release: ## Verify versions, .SRCINFO and PKGBUILD deps before tagging
	./packaging/check-release.sh

# ---------------------------------------------------------------------------

.PHONY: help
help: ## Show this help
	@grep -hE '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) \
		| sort \
		| awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-18s\033[0m %s\n", $$1, $$2}'
