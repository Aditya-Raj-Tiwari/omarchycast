import Quickshell
import Quickshell.Io
import Quickshell.Wayland
import Quickshell.Hyprland
import QtQuick
import qs.Commons
import qs.Ui

// Overlay entry point. Omarchy summons this with `omarchy-shell toggle
// io.github.aditya-raj-tiwari.omarchycast`; the contract is the open/close/toggle trio below.
Item {
  id: root

  property bool opened: false
  property bool settingsOpen: false
  property string queryText: ""
  property int selectedIndex: 0
  property var results: []
  property string statusMessage: ""
  // Hover must not steal the selection from the keyboard just because the
  // pointer happens to rest over a row. It only takes over once it actually moves.
  property bool pointerArmed: false
  // Set while a "Copied" confirmation is on screen, so the launcher stays up
  // just long enough for the user to see that something happened.
  property bool confirming: false
  /// Shown once, on the first open. Rows are real examples: pressing Enter puts
  /// one in the search field so the feature demonstrates itself, which teaches
  /// more than a page of text nobody reads.
  property bool tourActive: false
  // Guards against flashing the tour before the daemon's config has arrived —
  // the local default deliberately says "seen".
  property bool configLoaded: false
  // Resolved once per summon rather than bound live, so the launcher stays put
  // if focus moves while it is open.
  property var targetScreen: null

  // Mirrors the daemon's config.json. Populated on connect; the settings pane
  // edits this copy and sends the whole object back.
  property var config: ({
    hotkey: "CTRL + SPACE",
    providers: {
      apps: true, calculator: true, dates: true, notes: true, plugins: true, omarchy: true,
      appsLimit: 20, notesLimit: 8, pluginsLimit: 10, omarchyLimit: 8, notesDirectory: ""
    },
    appearance: {
      width: 720, rowsVisible: 8, cornerRadius: 16, followTheme: true,
      compact: false, fontScale: 100
    },
    behaviour: { hideOnBlur: true, escClearsFirst: true, showRecentWhenEmpty: true, tourSeen: true }
  })

  // Shares the [menu] surface tokens, so any theme that styles the Omarchy menu
  // styles the launcher too.
  readonly property bool themed: config.appearance.followTheme
  readonly property color background: themed ? Color.menu.background : "#16161d"
  readonly property color foreground: themed ? Color.menu.text : "#dcd7ba"
  readonly property color borderColor: themed ? Color.menu.border : "#2a2a35"
  readonly property color scrim: themed ? Color.menu.scrim : "#99000000"
  readonly property color selectedBackground: themed ? Color.menu.selectedBackground : "#2d4f67"
  readonly property color selectedText: themed ? Color.menu.selectedText : "#ffffff"
  readonly property string fontFamily: Style.font.menuFamily

  // Density and type scale. Every font size the theme provides passes through
  // fs(), and the paddings that give rows their height come from dense().
  readonly property bool compact: config.appearance.compact
  readonly property real fontScale: Math.max(0.7, Math.min(1.6, (config.appearance.fontScale || 100) / 100))
  function fs(px) { return Math.max(8, Math.round(px * root.fontScale)) }
  function dense(comfortable, tight) { return root.compact ? tight : comfortable }

  readonly property int cardWidth: Math.max(Style.space(420), Math.min(config.appearance.width, panel.width - Style.gapsOut * 2))
  readonly property int rowHeight: root.fs(Style.font.body) + root.fs(Style.font.caption)
    + root.dense(Style.space(22), Style.space(10))
  readonly property int headerHeight: Math.max(root.dense(Style.space(56), Style.space(44)),
    root.fs(Style.font.title) + root.dense(Style.spacing.controlPaddingY * 2, Style.space(10)))
  readonly property int footerHeight: root.dense(Style.space(38), Style.space(30))

  // ---------------------------------------------------------------- lifecycle

  // A keyboard-summoned launcher belongs on the output the user is looking at,
  // which on a multi-monitor setup is rarely the one Quickshell would pick.
  function resolveFocusedScreen() {
    var monitor = Hyprland.focusedMonitor
    var name = monitor ? String(monitor.name || "") : ""
    if (name === "") return null
    var screens = Quickshell.screens
    for (var i = 0; i < screens.length; i++) {
      if (String(screens[i].name) === name) return screens[i]
    }
    return null
  }

  function open(payloadJson) {
    root.targetScreen = root.resolveFocusedScreen()
    root.opened = true
    root.settingsOpen = false
    root.queryText = ""
    // The TextInput keeps its own text; clearing only the mirror leaves the
    // previous session's query on screen.
    input.text = ""
    root.selectedIndex = 0
    root.statusMessage = ""
    root.tourActive = false
    root.disarmPointer()
    daemon.ensureConnected()
    daemon.requestConfig()
    root.runQuery("")
    Qt.callLater(function () { input.forceActiveFocus() })
  }

  function close() {
    closeAfterConfirm.stop()
    clearConfirm.stop()
    root.confirming = false
    root.statusMessage = ""
    root.opened = false
    root.settingsOpen = false
    root.results = []
  }

  function toggle() {
    if (root.opened) root.close()
    else root.open("{}")
  }

  // The search field is hidden while settings are open, which would otherwise
  // leave nothing focused and no key handler to receive Escape.
  onSettingsOpenChanged: Qt.callLater(function () {
    if (root.settingsOpen) settingsPane.forceActiveFocus()
    else if (root.opened) input.forceActiveFocus()
  })

  // ------------------------------------------------------------------- search

  function runQuery(text) {
    if (root.tourActive) {
      if (text.length === 0) {
        root.results = root.tourItems
        return
      }
      root.endTour(false)
    }
    if (text.length === 0 && !config.behaviour.showRecentWhenEmpty) {
      root.results = root.syntheticFor("")
      root.selectedIndex = 0
      return
    }
    daemon.send({ op: "query", text: text })
  }

  // Built-in commands, matched in the overlay rather than the daemon because
  // they act on the shell rather than on anything the daemon indexes.
  readonly property var tourItems: [
    {
      id: "tour:calc", provider: "tour", kind: "Try", glyph: "=", icon: null,
      title: "1920 * 0.85", subtitle: "Maths, units, bases and percentages — ↵ copies the answer",
      accessory: "↵ try"
    },
    {
      id: "tour:units", provider: "tour", kind: "Try", glyph: "=", icon: null,
      title: "25 GB to MB", subtitle: "Convert units: also 5 miles to km, 0xff to decimal",
      accessory: "↵ try"
    },
    {
      id: "tour:date", provider: "tour", kind: "Try", glyph: "\u{1f5d3}", icon: null,
      title: "days until october 8", subtitle: "Ask about dates — also 30 days from now, 2 weeks ago",
      accessory: "↵ try"
    },
    {
      id: "tour:note", provider: "tour", kind: "Try", glyph: "\u{270e}", icon: null,
      title: "note Reading list", subtitle: "Create a markdown note and open it straight away",
      accessory: "↵ try"
    },
    {
      id: "tour:clipboard", provider: "tour", kind: "Try", glyph: "\u{2398}", icon: null,
      title: "clipboard", subtitle: "Open Omarchy's clipboard manager",
      accessory: "↵ try"
    },
    {
      id: "tour:settings", provider: "tour", kind: "Try", glyph: "\u{2699}", icon: null,
      title: "settings", subtitle: "Change the hotkey, sources, appearance and behaviour",
      accessory: "↵ try"
    }
  ]

  function startTour() {
    root.tourActive = true
    root.results = root.tourItems
    root.selectedIndex = 0
    root.disarmPointer()
  }

  /// Ends the tour. `dismissed` distinguishes deliberately finishing with it —
  /// skipping with Escape, or trying one of the examples — from merely starting
  /// to type, which hides the rows but must not count as having seen it. Marking
  /// it seen on the first keystroke is how it manages to never actually be read.
  function endTour(dismissed) {
    if (!root.tourActive) return
    root.tourActive = false
    if (!dismissed || root.config.behaviour.tourSeen) return
    var next = JSON.parse(JSON.stringify(root.config))
    next.behaviour.tourSeen = true
    root.config = next
    daemon.send({ op: "setConfig", config: next })
  }

  readonly property var commands: [
    {
      id: "ui:settings", provider: "ui", kind: "Omarchycast",
      title: "Omarchycast Settings", subtitle: "Hotkey, sources, appearance, behaviour",
      icon: null, glyph: "⚙", accessory: null,
      keywords: ["settings", "preferences", "omarchycast", "config", "hotkey"]
    },
    {
      id: "ui:clipboard", provider: "ui", kind: "Omarchy",
      title: "Clipboard History", subtitle: "Open Omarchy's clipboard manager",
      icon: null, glyph: "⎘", accessory: null,
      keywords: ["clipboard", "clip", "history", "paste"]
    },
    {
      id: "ui:tour", provider: "ui", kind: "Omarchycast",
      title: "What can Omarchycast do?", subtitle: "Replay the quick tour",
      icon: null, glyph: "?", accessory: null,
      keywords: ["tour", "help", "guide", "examples"]
    }
  ]

  function syntheticFor(text) {
    var needle = text.trim().toLowerCase()
    if (needle.length < 2) return []
    var matches = []
    for (var i = 0; i < root.commands.length; i++) {
      var command = root.commands[i]
      for (var k = 0; k < command.keywords.length; k++) {
        if (command.keywords[k].indexOf(needle) === 0) { matches.push(command); break }
      }
    }
    return matches
  }

  // Consumer-side bounds, applied even though the daemon clamps on its side:
  // this file is the display boundary, and it should hold on its own.
  readonly property int maxRows: 64
  readonly property int maxFieldChars: 400

  function sanitiseItems(items) {
    if (!items || !items.length) return []
    var out = []
    var count = Math.min(items.length, root.maxRows)
    for (var i = 0; i < count; i++) {
      var item = items[i]
      if (!item || typeof item.id !== "string") continue
      out.push({
        id: item.id.slice(0, 1024),
        provider: String(item.provider || "").slice(0, 32),
        kind: String(item.kind || "").slice(0, 32),
        title: String(item.title || "").slice(0, root.maxFieldChars),
        subtitle: item.subtitle ? String(item.subtitle).slice(0, root.maxFieldChars) : null,
        icon: item.icon ? String(item.icon).slice(0, 4096) : null,
        glyph: item.glyph ? String(item.glyph).slice(0, 8) : null,
        accessory: item.accessory ? String(item.accessory).slice(0, 64) : null
      })
    }
    return out
  }

  function applyResults(items) {
    // The empty-query request goes out before the config reply decides whether
    // to run the tour, so its response can land afterwards and overwrite it.
    if (root.tourActive) return
    root.results = root.syntheticFor(root.queryText).concat(root.sanitiseItems(items))
    root.selectedIndex = 0
    root.disarmPointer()
    resultList.positionViewAtBeginning()
  }

  // One definition of "back", used by every Escape handler: leave settings,
  // then clear the query, then dismiss.
  //
  // Not named `escape`: that is a JavaScript built-in, and QML rejects it with
  // "Illegal method name". The whole component then fails to compile while the
  // shell quietly keeps serving the previously compiled copy, which makes it
  // look like edits are being ignored rather than rejected.
  function goBack() {
    if (root.tourActive) {
      root.endTour(true)
      root.runQuery(input.text)
      return
    }
    if (root.settingsOpen) {
      root.settingsOpen = false
      return
    }
    if (root.config.behaviour.escClearsFirst && input.text.length > 0) {
      input.text = ""
      return
    }
    root.close()
  }

  function disarmPointer() {
    root.pointerArmed = false
  }

  function move(delta) {
    if (root.results.length === 0) return
    root.disarmPointer()
    // Wrapping means holding Up from the top lands on the last result.
    root.selectedIndex = (root.selectedIndex + delta + root.results.length) % root.results.length
    resultList.positionViewAtIndex(root.selectedIndex, ListView.Contain)
  }

  // Calculator, date and clipboard rows copy rather than launch, so the only
  // evidence anything happened is the confirmation we show here.
  function copiesToClipboard(item) {
    return item && (item.provider === "calc" || item.provider === "date")
  }

  function activate(action) {
    var item = root.results[root.selectedIndex]
    if (!item) return
    if (item.provider === "tour") {
      // Put the example in the field so it runs for real.
      root.endTour(true)
      input.text = item.title
      return
    }
    if (item.id === "ui:tour") {
      input.text = ""
      root.startTour()
      return
    }
    if (item.id === "ui:settings") {
      root.settingsOpen = true
      return
    }
    if (item.id === "ui:clipboard") {
      // Omarchy already ships a clipboard manager; opening it beats keeping a
      // second history of our own.
      root.close()
      openClipboard.running = true
      return
    }
    root.lastActionCopied = root.copiesToClipboard(item)
    daemon.send({ op: "activate", id: item.id, action: action })
  }

  property bool lastActionCopied: false

  function confirmCopy(thenClose) {
    root.confirming = true
    root.statusMessage = "Copied to clipboard"
    if (thenClose) closeAfterConfirm.restart()
    else clearConfirm.restart()
  }

  // -------------------------------------------------------------------- daemon

  function applyConfig(next) {
    if (!next) return
    root.config = next
    root.configLoaded = true
    if (root.opened && !root.tourActive && !next.behaviour.tourSeen
        && root.queryText.length === 0) {
      root.startTour()
    }
  }

  function saveConfig() {
    daemon.send({ op: "setConfig", config: root.config })
    root.statusMessage = "Saved"
  }

  // The socket lives behind a Loader because of how Quickshell's Socket fails:
  // it attempts to connect exactly once, when `connected: true` is first
  // applied, and after that attempt fails it ignores writes to `connected`
  // entirely — toggling the property retries nothing. The only way to try
  // again is a fresh Socket, so retrying means recreating the object. Without
  // this, a shell that loads the plugin before the daemon is listening (a
  // fresh install, most visibly) can never connect until the whole shell
  // restarts.
  QtObject {
    id: daemon

    readonly property bool connected: link.item ? link.item.connected : false

    function ensureConnected() {
      if (connected) return
      // Detached rather than a child Process: a daemon owned by the shell dies
      // with it on every shell restart, and the next shell then wakes up in
      // exactly the unconnected state this file exists to avoid. The daemon
      // refuses to bind a socket a live instance already owns, so a spare
      // start costs nothing.
      Quickshell.execDetached(["omarchycastd"])
      reconnect.restart()
    }

    function send(request) {
      if (!connected) {
        // Say so rather than showing an empty list, which reads as "nothing matched".
        root.statusMessage = "Starting the omarchycast daemon…"
        ensureConnected()
        return 0
      }
      return link.item.submit(request)
    }

    function requestConfig() {
      send({ op: "config" })
    }
  }

  Loader {
    id: link
    active: true

    sourceComponent: Socket {
      id: sock

      property int nextId: 1
      property var pending: ({})
      property int latestQueryId: 0

      path: Quickshell.env("XDG_RUNTIME_DIR") + "/omarchycast.sock"
      connected: true

      function submit(request) {
        var id = nextId++
        request.rid = id
        pending[id] = request.op
        if (request.op === "query") latestQueryId = id
        write(JSON.stringify(request) + "\n")
        flush()
        return id
      }

      onConnectedChanged: {
        if (!connected) return
        root.statusMessage = ""
        // Straight to submit, not through the facade: a unix-socket connect
        // can complete synchronously during instantiation, before the Loader
        // has assigned `item`, and the facade would misread that as "not
        // connected". The deferred query re-run runs after assignment.
        submit({ op: "config" })
        if (root.opened) Qt.callLater(function () {
          if (sock.connected) root.runQuery(root.queryText)
        })
      }

      parser: SplitParser {
        splitMarker: "\n"
        onRead: function (line) {
          var reply
          try {
            reply = JSON.parse(line)
          } catch (e) {
            return
          }

          var op = sock.pending[reply.rid]
          delete sock.pending[reply.rid]

          if (!reply.ok) {
            root.statusMessage = String(reply.error || "Something went wrong").slice(0, 300)
            return
          }

          if (op === "query") {
            // Drop responses to queries the user has already typed past.
            if (reply.rid !== sock.latestQueryId) return
            root.applyResults(reply.items)
          } else if (op === "config") {
            root.applyConfig(reply.config)
          } else if (op === "activate") {
            var stay = reply.outcome === "stay"
            if (root.lastActionCopied) root.confirmCopy(!stay)
            else if (!stay) root.close()
          }
        }
      }
    }
  }

  Timer {
    id: closeAfterConfirm
    interval: 550
    onTriggered: root.close()
  }

  Timer {
    id: clearConfirm
    interval: 1400
    onTriggered: {
      root.confirming = false
      root.statusMessage = ""
      if (root.opened && !root.settingsOpen) input.forceActiveFocus()
    }
  }

  Process {
    id: openClipboard
    command: ["omarchy-shell", "shell", "toggle", "omarchy.clipboard", "{}"]
    running: false
  }

  Timer {
    id: reconnect
    interval: 400
    repeat: true
    running: false
    property int attempts: 0
    onTriggered: {
      if (daemon.connected) {
        stop()
        attempts = 0
        return
      }
      // Recreate rather than toggle: a Socket that has failed once ignores
      // writes to `connected`, so a fresh object is the only real retry.
      link.active = false
      link.active = true
      attempts += 1
      if (attempts > 12) {
        stop()
        attempts = 0
        root.statusMessage = "Could not reach the omarchycast daemon"
      }
    }
  }

  // --------------------------------------------------------------------- view

  PanelWindow {
    id: panel
    screen: root.targetScreen
    visible: root.opened
    anchors { top: true; bottom: true; left: true; right: true }
    color: "transparent"
    WlrLayershell.namespace: "omarchycast"
    WlrLayershell.layer: WlrLayer.Overlay
    // Exclusive keyboard focus is what makes this feel like a real launcher:
    // no focus race, and no need to fight the compositor to keep the window up.
    WlrLayershell.keyboardFocus: WlrKeyboardFocus.Exclusive
    exclusionMode: ExclusionMode.Ignore

    Rectangle {
      anchors.fill: parent
      color: root.scrim
      MouseArea {
        anchors.fill: parent
        onClicked: if (root.config.behaviour.hideOnBlur) root.close()
      }
    }

    Rectangle {
      id: card
      anchors.centerIn: parent
      width: root.cardWidth
      height: Math.min(parent.height - Style.gapsOut * 2, root.headerHeight + contentHeight + root.footerHeight)
      radius: root.config.appearance.cornerRadius
      color: root.background
      border.color: root.borderColor
      border.width: 1
      clip: true

      readonly property int contentHeight: root.settingsOpen
        ? Math.min(Style.space(430), settingsPane.contentHeight)
        : Math.max(root.rowHeight, Math.min(root.results.length, root.config.appearance.rowsVisible) * root.rowHeight + Style.space(12))

      // Swallow clicks so they don't fall through to the dismissing scrim.
      MouseArea { anchors.fill: parent; onClicked: {} }

      // Safety net: whatever has focus, Escape always steps back one level.
      Keys.onPressed: function (event) {
        if (event.key !== Qt.Key_Escape) return
        root.goBack()
        event.accepted = true
      }

      Column {
        anchors.fill: parent

        // ------------------------------------------------------------- header
        Item {
          width: parent.width
          height: root.headerHeight

          Text {
            id: searchGlyph
            anchors.left: parent.left
            anchors.leftMargin: Style.space(20)
            anchors.verticalCenter: parent.verticalCenter
            text: root.settingsOpen ? "⚙" : "⌕"
            color: root.foreground
            opacity: 0.55
            font.family: root.fontFamily
            font.pixelSize: root.fs(Style.font.title)
          }

          TextInput {
            id: input
            anchors.left: searchGlyph.right
            anchors.leftMargin: Style.space(12)
            anchors.right: parent.right
            anchors.rightMargin: Style.space(20)
            anchors.verticalCenter: parent.verticalCenter
            visible: !root.settingsOpen
            focus: root.opened && !root.settingsOpen
            color: root.foreground
            font.family: root.fontFamily
            font.pixelSize: root.fs(Style.font.title)
            selectByMouse: true
            clip: true
            // Matches the daemon's MAX_QUERY_CHARS. Without this a very large
            // paste would exceed the request-line cap and cost the connection.
            maximumLength: 512

            onTextChanged: {
              root.queryText = text
              root.runQuery(text)
            }

            Text {
              anchors.fill: parent
              visible: input.text.length === 0
              text: "Search apps, calculate, or ask a date"
              color: root.foreground
              opacity: 0.45
              font: input.font
              verticalAlignment: Text.AlignVCenter
            }

            Keys.onPressed: function (event) {
              if (event.key === Qt.Key_Down || (event.key === Qt.Key_N && (event.modifiers & Qt.ControlModifier))) {
                root.move(1); event.accepted = true
              } else if (event.key === Qt.Key_Up || (event.key === Qt.Key_P && (event.modifiers & Qt.ControlModifier))) {
                root.move(-1); event.accepted = true
              } else if (event.key === Qt.Key_Return || event.key === Qt.Key_Enter) {
                root.activate((event.modifiers & Qt.ShiftModifier) ? "secondary" : "primary")
                event.accepted = true
              } else if (event.key === Qt.Key_Comma && (event.modifiers & Qt.ControlModifier)) {
                root.settingsOpen = true; event.accepted = true
              } else if (event.key === Qt.Key_Escape) {
                root.goBack()
                event.accepted = true
              }
            }
          }

          Text {
            anchors.left: searchGlyph.right
            anchors.leftMargin: Style.space(12)
            anchors.verticalCenter: parent.verticalCenter
            visible: root.settingsOpen
            text: "Omarchycast Settings"
            color: root.foreground
            font.family: root.fontFamily
            font.pixelSize: root.fs(Style.font.title)
          }
        }

        Rectangle {
          width: parent.width
          height: 1
          color: root.borderColor
          opacity: 0.6
        }

        // ------------------------------------------------------------ content
        Item {
          width: parent.width
          height: card.contentHeight

          ListView {
            id: resultList
            anchors.fill: parent
            anchors.topMargin: root.dense(Style.space(6), Style.space(3))
            anchors.bottomMargin: root.dense(Style.space(6), Style.space(3))
            visible: !root.settingsOpen
            model: root.results
            clip: true
            boundsBehavior: Flickable.StopAtBounds
            currentIndex: root.selectedIndex

            delegate: Item {
              id: row
              width: ListView.view.width
              height: root.rowHeight

              readonly property bool isSelected: index === root.selectedIndex
              readonly property bool isCalc: modelData.provider === "calc" || modelData.provider === "date"

              Rectangle {
                anchors.fill: parent
                anchors.leftMargin: Style.space(8)
                anchors.rightMargin: Style.space(8)
                radius: Style.space(10)
                color: row.isSelected ? root.selectedBackground : "transparent"
              }

              MouseArea {
                anchors.fill: parent
                hoverEnabled: true
                // Only a real movement arms the pointer; entering a row because
                // the list scrolled underneath a stationary cursor does not.
                onPositionChanged: {
                  root.pointerArmed = true
                  root.selectedIndex = index
                }
                onEntered: if (root.pointerArmed) root.selectedIndex = index
                onClicked: { root.selectedIndex = index; root.activate("primary") }
              }

              Row {
                anchors.fill: parent
                anchors.leftMargin: Style.space(20)
                anchors.rightMargin: Style.space(20)
                spacing: Style.space(12)

                Item {
                  width: Style.space(26)
                  height: parent.height

                  Image {
                    anchors.centerIn: parent
                    width: Style.space(26)
                    height: Style.space(26)
                    visible: status === Image.Ready
                    source: modelData.icon ? "file://" + modelData.icon : ""
                    fillMode: Image.PreserveAspectFit
                    sourceSize.width: Style.space(52)
                    sourceSize.height: Style.space(52)
                    asynchronous: true
                    cache: true
                  }

                  // Icon themes routinely lie about what they contain, so every
                  // row keeps a glyph to fall back to.
                  Text {
                    anchors.centerIn: parent
                    visible: !modelData.icon
                    text: modelData.glyph || ""
                    textFormat: Text.PlainText
                    color: row.isSelected ? root.selectedText : root.foreground
                    opacity: 0.6
                    font.family: root.fontFamily
                    font.pixelSize: root.fs(Style.font.body)
                  }
                }

                Column {
                  width: parent.width - Style.space(26) - meta.width - Style.space(24)
                  anchors.verticalCenter: parent.verticalCenter
                  spacing: Style.space(1)

                  Text {
                    width: parent.width
                    text: modelData.title || ""
                    textFormat: Text.PlainText
                    color: row.isSelected ? root.selectedText : root.foreground
                    elide: Text.ElideRight
                    font.family: root.fontFamily
                    // The answer is the point of a calculator row, so it gets weight.
                    font.pixelSize: root.fs(row.isCalc ? Style.font.title : Style.font.body)
                  }

                  Text {
                    width: parent.width
                    visible: !!modelData.subtitle
                    text: modelData.subtitle || ""
                    textFormat: Text.PlainText
                    color: row.isSelected ? root.selectedText : root.foreground
                    opacity: 0.6
                    elide: Text.ElideRight
                    font.family: root.fontFamily
                    font.pixelSize: root.fs(Style.font.caption)
                  }
                }

                Text {
                  id: meta
                  anchors.verticalCenter: parent.verticalCenter
                  text: modelData.accessory || modelData.kind || ""
                  textFormat: Text.PlainText
                  color: row.isSelected ? root.selectedText : root.foreground
                  opacity: 0.55
                  font.family: root.fontFamily
                  font.pixelSize: root.fs(Style.font.caption)
                }
              }
            }
          }

          Text {
            anchors.centerIn: parent
            visible: !root.settingsOpen && root.results.length === 0
            text: root.statusMessage.length > 0 ? root.statusMessage : "No results"
            textFormat: Text.PlainText
            color: root.foreground
            opacity: 0.5
            font.family: root.fontFamily
            font.pixelSize: root.fs(Style.font.body)
          }

          SettingsPane {
            id: settingsPane
            anchors.fill: parent
            visible: root.settingsOpen
            host: root
          }
        }

        Rectangle {
          width: parent.width
          height: 1
          color: root.borderColor
          opacity: 0.6
        }

        // ------------------------------------------------------------- footer
        Item {
          width: parent.width
          height: root.footerHeight

          Text {
            anchors.left: parent.left
            anchors.leftMargin: Style.space(16)
            anchors.verticalCenter: parent.verticalCenter
            text: "OMARCHYCAST"
            color: root.foreground
            opacity: 0.45
            font.family: root.fontFamily
            font.pixelSize: root.fs(Style.font.caption)
            font.letterSpacing: 1
          }

          Text {
            anchors.right: parent.right
            anchors.rightMargin: Style.space(16)
            anchors.verticalCenter: parent.verticalCenter
            color: root.foreground
            opacity: 0.55
            font.family: root.fontFamily
            font.pixelSize: root.fs(Style.font.caption)
            textFormat: Text.PlainText
            text: {
              if (root.confirming) return root.statusMessage
              if (root.settingsOpen) return "esc  Back"
              if (root.tourActive) return "↵  Try it      esc  Skip the tour"
              var item = root.results[root.selectedIndex]
              if (!item) return "ctrl+,  Settings      esc  Dismiss"
              var verb = root.copiesToClipboard(item) ? "Copy" : "Open"
              return "↵  " + verb + "      ctrl+,  Settings      esc  Dismiss"
            }
          }
        }
      }
    }
  }
}
