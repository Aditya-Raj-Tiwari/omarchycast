import QtQuick
import qs.Commons
import qs.Ui

// Settings for the launcher. `host` is the Omarchycast root, which owns the config
// object and the socket; this pane only edits and saves it.
Flickable {
  id: pane

  property var host

  readonly property color fg: host.foreground
  readonly property color accentColor: host.selectedBackground
  readonly property string fontFamily: host.fontFamily
  property bool capturing: false

  focus: true
  contentWidth: width
  contentHeight: layout.implicitHeight + Style.space(24)
  clip: true
  boundsBehavior: Flickable.StopAtBounds

  function commitTop(key, value) {
    var next = JSON.parse(JSON.stringify(host.config))
    next[key] = value
    host.config = next
    host.saveConfig()
  }

  function commit(section, key, value) {
    // QML won't notice a mutation inside the object, so the whole config is
    // rebuilt to force the bindings that read it to re-evaluate.
    var next = JSON.parse(JSON.stringify(host.config))
    next[section][key] = value
    host.config = next
    host.saveConfig()
  }

  // Qt delivers modifiers separately from the key, so a readable binding string
  // has to be assembled by hand.
  function keyName(code) {
    if (code >= Qt.Key_A && code <= Qt.Key_Z) return String.fromCharCode(65 + (code - Qt.Key_A))
    if (code >= Qt.Key_0 && code <= Qt.Key_9) return String.fromCharCode(48 + (code - Qt.Key_0))
    if (code >= Qt.Key_F1 && code <= Qt.Key_F12) return "F" + (1 + (code - Qt.Key_F1))
    switch (code) {
      case Qt.Key_Space: return "SPACE"
      case Qt.Key_Return:
      case Qt.Key_Enter: return "RETURN"
      case Qt.Key_Tab: return "TAB"
      case Qt.Key_Backspace: return "BACKSPACE"
      case Qt.Key_Period: return "PERIOD"
      case Qt.Key_Comma: return "COMMA"
      case Qt.Key_Slash: return "SLASH"
      default: return ""
    }
  }

  function describe(event) {
    var parts = []
    if (event.modifiers & Qt.MetaModifier) parts.push("SUPER")
    if (event.modifiers & Qt.ControlModifier) parts.push("CTRL")
    if (event.modifiers & Qt.AltModifier) parts.push("ALT")
    if (event.modifiers & Qt.ShiftModifier) parts.push("SHIFT")
    var name = pane.keyName(event.key)
    if (name === "") return ""
    parts.push(name)
    return parts.join(" + ")
  }

  // ------------------------------------------------------------- small pieces

  component SectionTitle: Text {
    color: pane.fg
    opacity: 0.5
    font.family: pane.fontFamily
    font.pixelSize: Style.font.caption
    font.letterSpacing: 1
    topPadding: Style.space(10)
  }

  component Toggle: Item {
    id: toggle
    property string label
    property string hint
    property bool checked
    signal toggled(bool value)

    width: parent ? parent.width : 0
    height: Math.max(Style.space(34), text.implicitHeight + Style.space(10))

    MouseArea {
      anchors.fill: parent
      onClicked: toggle.toggled(!toggle.checked)
    }

    Column {
      id: text
      anchors.left: parent.left
      anchors.right: box.left
      anchors.rightMargin: Style.space(12)
      anchors.verticalCenter: parent.verticalCenter
      spacing: Style.space(1)

      Text {
        width: parent.width
        text: toggle.label
        color: pane.fg
        elide: Text.ElideRight
        font.family: pane.fontFamily
        font.pixelSize: Style.font.body
      }
      Text {
        width: parent.width
        visible: !!toggle.hint
        text: toggle.hint || ""
        color: pane.fg
        opacity: 0.5
        elide: Text.ElideRight
        font.family: pane.fontFamily
        font.pixelSize: Style.font.caption
      }
    }

    Rectangle {
      id: box
      anchors.right: parent.right
      anchors.verticalCenter: parent.verticalCenter
      width: Style.space(38)
      height: Style.space(21)
      radius: height / 2
      color: toggle.checked ? pane.accentColor : "transparent"
      border.color: pane.fg
      border.width: 1
      opacity: toggle.checked ? 1.0 : 0.45

      Rectangle {
        width: Style.space(15)
        height: width
        radius: width / 2
        color: pane.fg
        anchors.verticalCenter: parent.verticalCenter
        x: toggle.checked ? parent.width - width - Style.space(3) : Style.space(3)
      }
    }
  }

  component StepButton: Rectangle {
    id: stepButton
    property string glyph
    signal activated

    width: Style.space(24)
    height: Style.space(24)
    radius: Style.space(6)
    color: "transparent"
    border.color: pane.fg
    border.width: 1
    opacity: 0.55

    Text {
      anchors.centerIn: parent
      text: stepButton.glyph
      color: pane.fg
      font.family: pane.fontFamily
      font.pixelSize: Style.font.caption
    }
    MouseArea { anchors.fill: parent; onClicked: stepButton.activated() }
  }

  component PathField: Item {
    id: field
    property string label
    property string value
    property string placeholder
    signal updated(string value)

    width: parent ? parent.width : 0
    height: Style.space(58)

    Text {
      anchors.left: parent.left
      anchors.top: parent.top
      text: field.label
      color: pane.fg
      font.family: pane.fontFamily
      font.pixelSize: Style.font.body
    }

    Rectangle {
      anchors.left: parent.left
      anchors.right: parent.right
      anchors.bottom: parent.bottom
      anchors.bottomMargin: Style.space(6)
      height: Style.space(28)
      radius: Style.space(8)
      color: "transparent"
      border.color: pane.fg
      border.width: 1
      opacity: entry.activeFocus ? 1.0 : 0.45

      TextInput {
        id: entry
        anchors.fill: parent
        anchors.leftMargin: Style.space(10)
        anchors.rightMargin: Style.space(10)
        verticalAlignment: TextInput.AlignVCenter
        text: field.value
        color: pane.fg
        font.family: pane.fontFamily
        font.pixelSize: Style.font.caption
        selectByMouse: true
        clip: true

        // Commit on Enter or on losing focus, never per keystroke — each commit
        // rewrites the config file and re-indexes.
        onEditingFinished: if (text !== field.value) field.updated(text)
        Keys.onPressed: function (event) {
          if (event.key === Qt.Key_Escape) {
            entry.text = field.value
            pane.forceActiveFocus()
            event.accepted = true
          }
        }

        Text {
          anchors.fill: parent
          visible: entry.text.length === 0
          text: field.placeholder
          color: pane.fg
          opacity: 0.4
          font: entry.font
          verticalAlignment: Text.AlignVCenter
        }
      }
    }
  }

  component Stepper: Item {
    id: stepper
    property string label
    property int value
    property int minimum: 0
    property int maximum: 9999
    property int step: 1
    signal updated(int value)

    width: parent ? parent.width : 0
    height: Style.space(34)

    function clamp(next) {
      return Math.max(stepper.minimum, Math.min(stepper.maximum, next))
    }

    Text {
      anchors.left: parent.left
      anchors.verticalCenter: parent.verticalCenter
      text: stepper.label
      color: pane.fg
      font.family: pane.fontFamily
      font.pixelSize: Style.font.body
    }

    Row {
      anchors.right: parent.right
      anchors.verticalCenter: parent.verticalCenter
      spacing: Style.space(4)

      StepButton {
        glyph: "−"
        onActivated: stepper.updated(stepper.clamp(stepper.value - stepper.step))
      }
      Text {
        width: Style.space(46)
        anchors.verticalCenter: parent.verticalCenter
        horizontalAlignment: Text.AlignHCenter
        text: stepper.value
        color: pane.fg
        font.family: pane.fontFamily
        font.pixelSize: Style.font.body
      }
      StepButton {
        glyph: "+"
        onActivated: stepper.updated(stepper.clamp(stepper.value + stepper.step))
      }
    }
  }

  // ------------------------------------------------------------------ content

  Column {
    id: layout
    width: pane.width - Style.space(40)
    x: Style.space(20)
    spacing: Style.space(2)

    SectionTitle { text: "HOTKEY" }

    Item {
      width: parent.width
      height: Style.space(40)

      Text {
        anchors.left: parent.left
        anchors.verticalCenter: parent.verticalCenter
        text: pane.capturing ? "Press the new combination…" : "Opens the launcher"
        color: pane.fg
        opacity: pane.capturing ? 1.0 : 0.5
        font.family: pane.fontFamily
        font.pixelSize: Style.font.body
      }

      Rectangle {
        id: capture
        anchors.right: parent.right
        anchors.verticalCenter: parent.verticalCenter
        width: Math.max(Style.space(150), keyLabel.implicitWidth + Style.space(24))
        height: Style.space(30)
        radius: Style.space(8)
        color: pane.capturing ? pane.accentColor : "transparent"
        border.color: pane.fg
        border.width: 1
        opacity: pane.capturing ? 1.0 : 0.7

        Text {
          id: keyLabel
          anchors.centerIn: parent
          text: host.config.hotkey
          color: pane.fg
          font.family: pane.fontFamily
          font.pixelSize: Style.font.caption
        }

        MouseArea {
          anchors.fill: parent
          onClicked: {
            pane.capturing = true
            capture.forceActiveFocus()
          }
        }

        focus: pane.capturing
        Keys.onPressed: function (event) {
          if (!pane.capturing) return
          if (event.key === Qt.Key_Escape) {
            pane.capturing = false
            event.accepted = true
            return
          }
          var combo = pane.describe(event)
          // Ignore bare modifier presses; wait for a real key.
          if (combo === "") { event.accepted = true; return }
          pane.capturing = false
          pane.commitTop("hotkey", combo)
          event.accepted = true
        }
      }
    }

    Text {
      width: parent.width
      wrapMode: Text.WordWrap
      text: "Wayland has no client-side global hotkey, so this writes the binding into ~/.config/hypr/omarchycast.lua and reloads Hyprland."
      color: pane.fg
      opacity: 0.45
      font.family: pane.fontFamily
      font.pixelSize: Style.font.caption
    }

    SectionTitle { text: "SOURCES" }

    Toggle {
      label: "Applications"; hint: "Search and launch desktop entries"
      checked: host.config.providers.apps
      onToggled: pane.commit("providers", "apps", value)
    }
    Stepper {
      label: "Application results"; value: host.config.providers.appsLimit
      minimum: 1; maximum: 40
      onUpdated: pane.commit("providers", "appsLimit", value)
    }
    Toggle {
      label: "Calculator"; hint: "Arithmetic and unit conversion"
      checked: host.config.providers.calculator
      onToggled: pane.commit("providers", "calculator", value)
    }
    Toggle {
      label: "Dates"; hint: "\"days until october 8\" and similar"
      checked: host.config.providers.dates
      onToggled: pane.commit("providers", "dates", value)
    }
    Toggle {
      label: "Notes"; hint: "Search markdown notes and open them in shadow-notes"
      checked: host.config.providers.notes
      onToggled: pane.commit("providers", "notes", value)
    }
    Stepper {
      label: "Note results"; value: host.config.providers.notesLimit
      minimum: 1; maximum: 40
      onUpdated: pane.commit("providers", "notesLimit", value)
    }
    PathField {
      label: "Notes folder"
      placeholder: "~/Notes"
      value: host.config.providers.notesDirectory
      onUpdated: pane.commit("providers", "notesDirectory", value)
    }

    SectionTitle { text: "APPEARANCE" }

    Stepper {
      label: "Width"; value: host.config.appearance.width
      minimum: 420; maximum: 1200; step: 20
      onUpdated: pane.commit("appearance", "width", value)
    }
    Stepper {
      label: "Rows shown"; value: host.config.appearance.rowsVisible
      minimum: 3; maximum: 16
      onUpdated: pane.commit("appearance", "rowsVisible", value)
    }
    Stepper {
      label: "Corner radius"; value: host.config.appearance.cornerRadius
      minimum: 0; maximum: 32; step: 2
      onUpdated: pane.commit("appearance", "cornerRadius", value)
    }
    Toggle {
      label: "Follow the Omarchy theme"; hint: "Off uses a fixed dark palette"
      checked: host.config.appearance.followTheme
      onToggled: pane.commit("appearance", "followTheme", value)
    }

    SectionTitle { text: "BEHAVIOUR" }

    Toggle {
      label: "Dismiss when clicking away"
      checked: host.config.behaviour.hideOnBlur
      onToggled: pane.commit("behaviour", "hideOnBlur", value)
    }
    Toggle {
      label: "Escape clears before dismissing"
      checked: host.config.behaviour.escClearsFirst
      onToggled: pane.commit("behaviour", "escClearsFirst", value)
    }
    Toggle {
      label: "Show frequent apps when empty"
      checked: host.config.behaviour.showRecentWhenEmpty
      onToggled: pane.commit("behaviour", "showRecentWhenEmpty", value)
    }
  }

  Keys.onPressed: function (event) {
    if (event.key === Qt.Key_Escape && !pane.capturing) {
      host.goBack()
      event.accepted = true
    }
  }
}
