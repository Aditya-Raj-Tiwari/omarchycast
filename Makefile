PREFIX ?= $(HOME)/.local
BINDIR ?= $(PREFIX)/bin
PLUGIN_ID ?= io.github.aditya-raj-tiwari.omarchycast
PLUGINDIR ?= $(HOME)/.config/omarchy/plugins/$(PLUGIN_ID)

.PHONY: all build install uninstall test check clean dev

all: build

build:
	cd daemon && cargo build --release

test:
	cd daemon && cargo test

QMLLINT ?= /usr/lib/qt6/bin/qmllint
OMARCHY_PATH ?= /usr/share/omarchy

check:
	cd daemon && cargo clippy --all-targets -- -D warnings
	omarchy plugin validate .
	# Catches QML that fails to compile — which the shell reports only in the
	# journal, while silently continuing to serve the previously compiled copy.
	$(QMLLINT) -I $(OMARCHY_PATH)/shell Omarchycast.qml SettingsPane.qml

NOTESDIR ?= $(HOME)/.local/share/omacastnotes

# Installs the daemon, the notes viewer, and registers the overlay.
install: build
	install -Dm755 daemon/target/release/omarchycastd $(BINDIR)/omarchycastd
	install -Dm755 notesapp/omacastnotes $(BINDIR)/omacastnotes
	mkdir -p $(NOTESDIR)
	cp -f notesapp/omacastnotes.py $(NOTESDIR)/
	cp -rf notesapp/ui $(NOTESDIR)/
	install -Dm644 notesapp/omacastnotes.desktop $(HOME)/.local/share/applications/omacastnotes.desktop
	mkdir -p $(PLUGINDIR)
	cp -f manifest.json Omarchycast.qml SettingsPane.qml $(PLUGINDIR)/
	omarchy-shell shell rescanPlugins || true
	@echo "Installed. Bind a key with: omarchycastd hotkey 'CTRL + SPACE'"

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
#     journalctl --user -e | grep omarchycast
dev:
	mkdir -p $(PLUGINDIR)
	cp -f manifest.json Omarchycast.qml SettingsPane.qml $(PLUGINDIR)/
	omarchy-restart-shell

uninstall:
	rm -f $(BINDIR)/omarchycastd $(BINDIR)/omacastnotes
	rm -rf $(NOTESDIR)
	rm -f $(HOME)/.local/share/applications/omacastnotes.desktop
	rm -rf $(PLUGINDIR)
	rm -f $(HOME)/.config/hypr/omarchycast.lua
	@echo "Remove the two 'Added by omarchycast' lines from ~/.config/hypr/hyprland.lua to finish."

clean:
	cd daemon && cargo clean
