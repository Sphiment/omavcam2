// Prototype C: the preview as a real window, with anchor snapping and no drag
// implementation at all.
//
// Dragging is the compositor's job — Super+drag moves this like any other
// window. Snapping is nine named positions you jump to. Dropping drag-snapping
// deletes the cursor polling, the position tracking, the drag-end guessing and
// the nearest-anchor search, because a chosen anchor needs none of them.
//
// Keys: 1-9 anchor (reading order, 1 = top-left) · +/- size · esc quit
import Quickshell
import Quickshell.Io
import Quickshell.Hyprland
import QtQuick
import QtMultimedia
import qs.Ui
import qs.Commons

ShellRoot {
  id: root

  readonly property string node: "/dev/video42"
  readonly property string winTitle: "omavcam preview"
  readonly property var sizes: ["small", "medium", "large", "original"]

  property int sizeIndex: 0
  property bool bound: false
  property real nativeW: 1280
  property real nativeH: 720
  property var monitor: null

  FloatingWindow {
    id: win
    title: root.winTitle
    color: Color.background
    implicitWidth: root.previewSize().width
    implicitHeight: root.previewSize().height

    MediaDevices { id: devices }
    Camera { id: cam }
    CaptureSession { camera: cam; videoOutput: out }
    VideoOutput { id: out; anchors.fill: parent; fillMode: VideoOutput.PreserveAspectFit }

    // Only listens until the stream's real size is known, then stops — there is
    // no per-frame work in the steady state.
    Connections {
      target: out.videoSink
      enabled: root.bound && root.nativeW === 1280 && root.nativeH === 720
      function onVideoFrameChanged() {
        const f = out.videoSink.videoFrame
        if (f && f.width > 0) { root.nativeW = f.width; root.nativeH = f.height }
      }
    }

    Text {
      anchors.centerIn: parent
      visible: !root.bound
      color: Color.foreground
      font.family: Style.font.family
      font.pixelSize: 13
      horizontalAlignment: Text.AlignHCenter
      text: "waiting for " + root.node + "\nrun:  ./phone start"
    }

    // focus: true alone only sets focus within the scope — the item has to be
    // given active focus once the window exists, or key events go nowhere.
    FocusScope {
      id: keys
      anchors.fill: parent
      focus: true
      Component.onCompleted: forceActiveFocus()
      Keys.onPressed: (e) => {
        if (e.key >= Qt.Key_1 && e.key <= Qt.Key_9) root.snap(e.key - Qt.Key_1)
        else if (e.key === Qt.Key_Equal || e.key === Qt.Key_Plus)
          root.sizeIndex = Math.min(root.sizes.length - 1, root.sizeIndex + 1)
        else if (e.key === Qt.Key_Minus)
          root.sizeIndex = Math.max(0, root.sizeIndex - 1)
        else if (e.key === Qt.Key_Escape) Qt.quit()
      }
    }

    // Click to take focus back, and show whether it has it — with no border
    // there is otherwise no way to tell why the keys are dead.
    MouseArea {
      anchors.fill: parent
      onClicked: keys.forceActiveFocus()
    }

    Rectangle {
      anchors { left: parent.left; bottom: parent.bottom; margins: 4 }
      width: hint.width + 12; height: hint.height + 8
      color: Qt.rgba(Color.background.r, Color.background.g, Color.background.b, 0.85)
      radius: Style.cornerRadius
      Text {
        id: hint
        anchors.centerIn: parent
        color: Color.foreground
        font.family: Style.font.family
        font.pixelSize: 11
        text: root.sizes[root.sizeIndex] + "  ·  "
              + (keys.activeFocus ? "1-9 to anchor" : "click first — no focus")
      }
    }
  }

  function previewSize() {
    const ar = nativeW / nativeH
    if (sizes[sizeIndex] === "original") return Qt.size(nativeW, nativeH)
    const h = Math.round((monitor ? monitor.h : 900)
                         * (sizes[sizeIndex] === "small" ? 0.18
                          : sizes[sizeIndex] === "medium" ? 0.28 : 0.40))
    return Qt.size(Math.round(h * ar), h)
  }

  // videoInputs[].id is a QByteArray; `===` against a string literal is
  // silently false and the session falls back to the laptop webcam.
  function bind() {
    for (let i = 0; i < devices.videoInputs.length; i++) {
      if (String(devices.videoInputs[i].id) === node) {
        cam.cameraDevice = devices.videoInputs[i]
        cam.active = true
        bound = true
        return
      }
    }
  }

  // The node only enumerates while something writes to it, so this retries —
  // but only while unbound. Nothing polls once the stream is up.
  Timer {
    interval: 1000
    running: !root.bound
    repeat: true
    onTriggered: root.bind()
  }

  // Anchoring targets the focused monitor, so we never need to know where the
  // window currently is — which is what let all the position tracking go.
  property int pendingAnchor: -1
  function snap(i) {
    pendingAnchor = i
    if (!monitors.running) monitors.running = true
  }

  Process {
    id: monitors
    command: ["hyprctl", "monitors", "-j"]
    stdout: StdioCollector {
      onStreamFinished: {
        let m = null
        try {
          for (const d of JSON.parse(text)) if (d.focused) m = d
        } catch (e) { return }
        if (!m) return

        // hyprctl reports position and reserved in logical pixels but size in
        // physical ones, so the scale has to be divided out.
        const r = m.reserved
        root.monitor = {
          x: m.x + r[0], y: m.y + r[1],
          w: m.width / m.scale - r[0] - r[2],
          h: m.height / m.scale - r[1] - r[3]
        }
        if (root.pendingAnchor < 0) return

        const i = root.pendingAnchor
        root.pendingAnchor = -1
        const g = Style.gapsOut
        const u = root.monitor
        const col = i % 3, row = Math.floor(i / 3)
        const x = col === 0 ? u.x + g
                : col === 1 ? u.x + (u.w - win.width) / 2
                : u.x + u.w - win.width - g
        const y = row === 0 ? u.y + g
                : row === 1 ? u.y + (u.h - win.height) / 2
                : u.y + u.h - win.height - g
        Hyprland.dispatch('hl.dsp.window.move({ window = "title:^(' + root.winTitle
                          + ')$", x = ' + Math.round(x) + ', y = ' + Math.round(y) + ' })')
      }
    }
  }

  // Omarchy wraps Hyprland in Lua, so window rules go through `eval`, not
  // `keyword`. Runs once; the monitor read that follows seeds the size presets.
  Process {
    running: true
    command: ["bash", "-c",
      "hyprctl eval 'o.window({ title = \"^(omavcam preview)$\" }, " +
      "{ float = true, pin = true, no_dim = true, border_size = 0, " +
      "opacity = \"1 1\", tag = \"-default-opacity\" })' >/dev/null"]
    onExited: root.snap(0)
  }

  Component.onCompleted: bind()
}
