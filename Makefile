PREFIX ?= $(HOME)/.local
BINDIR ?= $(PREFIX)/bin
PLUGINDIR ?= $(HOME)/.config/omarchy/plugins/adityarajtiwari.omacast

.PHONY: all build install uninstall test check clean dev

all: build

build:
	cd daemon && cargo build --release

test:
	cd daemon && cargo test

check:
	cd daemon && cargo clippy --all-targets -- -D warnings
	omarchy plugin validate .

# Installs the daemon and registers the overlay with the shell.
install: build
	install -Dm755 daemon/target/release/omacastd $(BINDIR)/omacastd
	mkdir -p $(PLUGINDIR)
	cp -f manifest.json Omacast.qml SettingsPane.qml $(PLUGINDIR)/
	omarchy-shell shell rescanPlugins || true
	@echo "Installed. Bind a key with: omacastd hotkey 'CTRL + SPACE'"

# Fast UI iteration: sync the QML and reload the shell without rebuilding Rust.
# The plugin loader does not follow symlinks, so the files are copied.
dev:
	mkdir -p $(PLUGINDIR)
	cp -f manifest.json Omacast.qml SettingsPane.qml $(PLUGINDIR)/
	omarchy-shell shell rescanPlugins || true

uninstall:
	rm -f $(BINDIR)/omacastd
	rm -rf $(PLUGINDIR)
	rm -f $(HOME)/.config/hypr/omacast.lua
	@echo "Remove the two 'Added by omacast' lines from ~/.config/hypr/hyprland.lua to finish."

clean:
	cd daemon && cargo clean
