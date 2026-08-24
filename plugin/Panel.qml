import QtQuick
import Quickshell
import Quickshell.Io
import qs.Commons
import qs.Ui

// The bar widget and the panel behind it.
//
// This file holds no state and makes no system calls. It opens the daemon's
// socket, renders what it is pushed, and sends requests; every `scrcpy`,
// `adb` and `hyprctl` invocation in this project lives in the daemon
// (ADR-0001). Connecting is also what makes the daemon exist: the listening
// socket is systemd's, and something connecting to it is the only thing that
// starts the service behind it.
//
// The panel is deliberately thin — a light, a switch, and the phones whenever
// there is more than one to point at (ADR-0007). Settings live in Studio,
// never here: the frequent action has to be instant.
Panel {
  id: root
  moduleName: "omavcam"
  ipcTarget: "omavcam"

  // The protocol the daemon speaks, from src/protocol.rs. A mismatch is
  // reported rather than misparsed.
  readonly property int protocol: 3

  // The whole state, exactly as pushed, or null while we have not been told.
  // Not `state`: every QML Item already has one of those.
  property var daemonState: null

  // Whether the socket has actually failed, as opposed to not having answered
  // yet. Without it the widget would show trouble for the frame between
  // connecting and the first push, which is trouble that is not happening.
  property bool unreachable: false

  // The id of the request we are waiting on, or "". A switch and a picker
  // cannot generate more than one at a time.
  property string pending: ""
  property int nextId: 1

  // What the daemon last refused, in its own words. The panel shows it; the
  // bar does not, because a refused request is not a broken setup.
  property string refusal: ""

  readonly property bool linked: daemonSocket.connected && daemonState !== null
  readonly property bool capturing: !!(daemonState && daemonState.capture)
  readonly property string connectionState: daemonState ? daemonState.connection.state : ""

  // What the bar has to warn about: something is wrong and only the person at
  // the desk can fix it. No phone attached is not trouble, it is Tuesday.
  readonly property bool troubled: daemonState
    ? (!daemonState.adb_ok
      || connectionState === "unauthorised"
      || connectionState === "pairing_failed"
      || connectionState === "unreachable")
    : unreachable

  // The phone the connection names, in whatever phase it is in.
  readonly property string selectedSerial: {
    var connection = daemonState ? daemonState.connection : null
    return connection && connection.phone ? connection.phone.serial : ""
  }

  // The phones worth offering. One phone that is already the one in use is no
  // choice at all; anything else is — two on the desk, or one that has never
  // been picked because a different phone is the remembered one.
  //
  // Read from the state's own list rather than from `Unselected.available`,
  // which is the same phones and goes away when the protocol version next
  // moves.
  readonly property var choices: {
    if (!daemonState) return []
    var choices = (daemonState.attached || []).slice()
    var knownPhones = daemonState.known || []
    knownPhones.forEach(function (known) {
      if (known.transport === "wireless"
          && !choices.some(function (item) {
            return item.phone.serial === known.phone.serial
              || item.phone.serial === known.hardware_id
          })) {
        choices.push({"phone": known.phone, "authorised": true})
      }
    })
    if (choices.length === 1 && choices[0].phone.serial === selectedSerial) return []
    return choices
  }

  // What the picker has to say before someone clicks it, not after.
  function pickerNote() {
    var notes = []
    if (capturing) notes.push("Switching stops the capture")
    if (choices.some(function (phone) { return phone.authorised === false }))
      notes.push("A dimmed phone has not accepted the debugging prompt")
    return notes.join(" · ")
  }

  readonly property color foreground: bar ? bar.foreground : Color.foreground
  readonly property color urgent: bar ? bar.urgent : Color.urgent
  readonly property string fontFamily: bar ? bar.fontFamily : Style.font.family

  // ---- the daemon --------------------------------------------------------

  function send(kind, serial) {
    if (!daemonSocket.connected || pending !== "") return
    refusal = ""
    var request = {"v": protocol, "id": String(nextId++), "kind": kind}
    if (serial !== undefined) request.serial = serial
    pending = request.id
    daemonSocket.write(JSON.stringify(request) + "\n")
    daemonSocket.flush()
  }

  function receive(line) {
    var message
    // The socket is the daemon's and mode 0600, but a line we cannot parse
    // must not take the widget down with it.
    try {
      message = JSON.parse(line)
    } catch (e) {
      return
    }
    if (message.v !== protocol) {
      refusal = "the daemon speaks protocol " + message.v + ", this widget speaks " + protocol
      return
    }
    if (message.type === "state") {
      daemonState = message.state
    } else if (message.type === "response" && message.id === pending) {
      pending = ""
      refusal = message.ok ? "" : message.error.message
    }
  }

  function toggleCapture() {
    send(capturing ? "stop" : "start")
  }

  // ---- what all that says ------------------------------------------------

  function connectionWords() {
    if (!daemonSocket.connected) return "Daemon unreachable"
    if (!daemonState) return "Waiting for the daemon"
    if (!daemonState.adb_ok) return "adb unavailable"
    var connection = daemonState.connection
    if (connection.state === "no_phone") return "No phone"
    if (connection.state === "unselected") return daemonState.attached.length + " phones attached, none chosen"
    if (connection.state === "unauthorised") return connection.phone.name + " has not accepted the debugging prompt"
    if (connection.state === "connecting") return "Connecting to " + connection.phone.name
    if (connection.state === "connected") return connection.phone.name + " connected"
    if (connection.state === "needs_pairing") return "Wireless pairing needed — run omavcam pair"
    if (connection.state === "pairing_failed") {
      if (connection.reason === "wrong_code") return "Wireless pairing failed — wrong code"
      if (connection.reason === "wrong_address") return "Wireless pairing failed — wrong pairing address"
      return "Wireless pairing failed — phone may be on a different network"
    }
    if (connection.state === "unreachable")
      return connection.phone.name + " unreachable — check the network or connect port"
    return connection.state
  }

  function captureWords() {
    if (capturing) return daemonState.capture.size + " from " + daemonState.capture.phone.name
    return "Off — omavcam is in no camera list"
  }

  function tooltipWords() {
    if (capturing) return "omavcam — capturing from " + daemonState.capture.phone.name
    return "omavcam — " + connectionWords()
  }

  // nf-md-video (U+F03D), nf-fa-warning (U+F071), nf-md-video_off (U+F0568):
  // a running capture, something only the person at the desk can fix, and off.
  readonly property string icon: capturing ? "" : (troubled ? "" : "󰕨")
  readonly property color light: capturing ? Color.accent : (troubled ? urgent : Qt.darker(foreground, 1.8))

  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight

  Socket {
    id: daemonSocket
    // Connecting is what socket-activates the daemon, so the widget holds the
    // link open whether the panel is showing or not — the bar has to know
    // about a capture nobody started from here.
    connected: true
    path: Quickshell.env("OMAVCAM_SOCKET")
      || (Quickshell.env("XDG_RUNTIME_DIR") || "/tmp") + "/omavcam.sock"

    parser: SplitParser {
      splitMarker: "\n"
      onRead: function(line) { root.receive(line) }
    }

    onError: root.unreachable = true

    // A daemon that goes away leaves nothing to render and no answer coming.
    // Forgetting both is what keeps the widget from wedging.
    onConnectionStateChanged: {
      if (connected) {
        root.unreachable = false
        retry.interval = retry.firstInterval
      } else {
        root.daemonState = null
        root.pending = ""
      }
    }
  }

  // Reconnecting is the whole recovery: the daemon pushes its state on
  // connect, so there is nothing to resync. The wait doubles because a daemon
  // that cannot start at all must not be asked to twice a second forever —
  // socket activation would answer every attempt with another failed start.
  Timer {
    id: retry
    readonly property int firstInterval: 2000
    interval: firstInterval
    repeat: true
    running: !daemonSocket.connected
    onTriggered: {
      daemonSocket.connected = true
      interval = Math.min(interval * 2, 30000)
    }
  }

  // The daemon answers every request, so silence this long means it will not.
  // Matches the CLI's own patience.
  Timer {
    interval: 20000
    running: root.pending !== ""
    onTriggered: {
      root.pending = ""
      root.refusal = "the daemon did not answer"
    }
  }

  BarIconButton {
    id: button
    anchors.fill: parent
    bar: root.bar
    text: root.icon
    active: root.capturing || root.troubled
    activeColor: root.capturing ? Color.accent : root.urgent
    tooltipText: root.tooltipWords()
    onPressed: function(b) { root.toggle() }
  }

  KeyboardPanel {
    id: panel
    anchorItem: button
    owner: root
    bar: root.bar
    open: root.opened
    focusTarget: keyCatcher
    contentWidth: panel.fittedContentWidth(Style.space(320))
    contentHeight: panel.fittedContentHeight(column.implicitHeight)

    PanelKeyCatcher {
      id: keyCatcher
      anchors.fill: parent
      onActivateRequested: root.toggleCapture()
      onCloseRequested: root.close()
      onTabRequested: function(direction) { root.switchPanel(direction) }

      Column {
        id: column
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.top: parent.top
        spacing: Style.space(14)

        // ---------- the light, and what it is saying ----------
        PanelHero {
          title: "omavcam"
          meta: root.connectionWords()
          foreground: root.foreground
          fontFamily: root.fontFamily

          iconComponent: Rectangle {
            width: Style.space(12)
            height: width
            radius: width / 2
            color: root.light

            Behavior on color { ColorAnimation { duration: 200 } }
          }
        }

        // ---------- the switch ----------
        Toggle {
          width: parent.width
          label: "Capture"
          description: root.captureWords()
          checked: root.capturing
          foreground: root.foreground
          fontFamily: root.fontFamily
          enabled: root.linked
          opacity: enabled ? 1 : 0.5
          onClicked: root.toggleCapture()
        }

        // ---------- the picker, only while there is a choice ----------
        Column {
          width: parent.width
          spacing: Style.space(8)
          visible: root.choices.length > 0

          PanelSeparator { foreground: root.foreground }

          PanelSectionHeader {
            text: "PHONES"
            foreground: root.foreground
            fontFamily: root.fontFamily
          }

          // One line explains every dimmed row, and warns about the one
          // click in this panel that takes something away.
          Text {
            width: parent.width
            text: root.pickerNote()
            visible: text !== ""
            color: root.capturing ? root.urgent : Qt.darker(root.foreground, 1.4)
            font.family: root.fontFamily
            font.pixelSize: Style.font.caption
            wrapMode: Text.WordWrap
          }

          Repeater {
            model: root.choices

            Button {
              required property var modelData
              width: parent.width
              text: modelData.phone.name
              iconText: "" // nf-fa-mobile
              leftAlign: true
              // The one in use, so a picker offered in every state still says
              // which phone the webcam is pointed at.
              selected: modelData.phone.serial === root.selectedSerial
              // Dimmed, not disabled: selecting it is how the panel comes to
              // say which phone needs the prompt accepted, and a row that
              // cannot be clicked is a dead end instead of an instruction.
              opacity: modelData.authorised === false ? 0.55 : 1
              foreground: root.foreground
              fontFamily: root.fontFamily
              tooltipText: modelData.phone.serial
              onClicked: root.send("select", modelData.phone.serial)
            }
          }
        }

        // ---------- what the daemon refused ----------
        Text {
          width: parent.width
          visible: root.refusal !== ""
          text: root.refusal
          color: root.urgent
          font.family: root.fontFamily
          font.pixelSize: Style.font.caption
          wrapMode: Text.WordWrap
        }
      }
    }
  }
}
