PREFIX ?= $(HOME)/.local
BINDIR ?= $(PREFIX)/bin
PLUGIN_ID ?= adityarajtiwari.omacast
PLUGINDIR ?= $(HOME)/.config/omarchy/plugins/$(PLUGIN_ID)

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

# Sync the QML and reload it.
#
# Qt caches compiled QML by URL, so neither `rescanPlugins` nor toggling the
# plugin's enabled bit picks up an edit — the shell keeps serving the previously
# compiled component. Restarting the shell is the only reliable reload. Note
# `omarchy-restart-shell`, NOT `omarchy-refresh-shell`: the latter resets
# shell.json to Omarchy defaults and would discard the user's bar layout.
#
# If a change still seems to have no effect, the QML failed to compile and the
# old component is still live. That failure is silent in the UI:
#     journalctl --user -e | grep omacast
dev:
	mkdir -p $(PLUGINDIR)
	cp -f manifest.json Omacast.qml SettingsPane.qml $(PLUGINDIR)/
	omarchy-restart-shell

uninstall:
	rm -f $(BINDIR)/omacastd
	rm -rf $(PLUGINDIR)
	rm -f $(HOME)/.config/hypr/omacast.lua
	@echo "Remove the two 'Added by omacast' lines from ~/.config/hypr/hyprland.lua to finish."

clean:
	cd daemon && cargo clean
