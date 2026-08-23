// THROWAWAY PROTOTYPE — DO NOT COPY THIS INTO THE REAL PLUGIN.
//
// This exists to prove the ADRs, not to be the implementation. It was written
// to be run and thrown away: it talks to hyprctl and scrcpy directly instead of
// through the daemon, hardcodes the things the daemon owns, handles no errors,
// and persists nothing. Read it for the SHAPE. Then write the real one
// properly. See prototypes/README.md for the specific things that must not
// survive.
//
// What it demonstrates (ADR-0013): the preview is SCRCPY'S OWN WINDOW. One
// scrcpy process decodes once, draws that window, and writes the virtual
// camera — so the preview and a video call can be live at the same time, which
// is impossible when both must READ the node (ADR-0011: one streaming client
// per V4L2 device, real webcams included).
//
// Nothing here renders video. This shell only launches scrcpy, applies window
// rules to the window it opens, and moves it. Measured 24.0% CPU against
// 18-23% for scrcpy with no window at all — the drawing is OpenGL and close to
// free, where fanning out to a second node cost 32.2%.
//
// Dragging is still the compositor's job — Super+drag moves it like any other
// window, and snapping is nine named positions you jump to.
//
// Drive it over IPC (see the IpcHandler below) — there is no window of ours.
import Quickshell
import Quickshell.Io
import Quickshell.Hyprland
import QtQuick
import qs.Ui
import qs.Commons

ShellRoot {
  id: root

  readonly property string node: "/dev/video42"
  readonly property string winTitle: "omavcam preview"
  readonly property var sizes: ["small", "medium", "large", "original"]

  property int sizeIndex: 1
  property bool capturing: false
  // The capture's real size. Hardcoded because the daemon owns it; the shape to
  // copy is that the preview's aspect follows the capture, not the other way.
  readonly property real nativeW: 1280
  readonly property real nativeH: 720
  property var monitor: null
  property int pendingAnchor: -1
  property bool pendingSize: false

  // --no-control matters: without it the window forwards every click and
  // keystroke to the phone. --window-title is what the rules and moves match on.
  Process {
    id: capture
    running: true
    command: ["scrcpy",
      "--video-source=camera", "--camera-id=0",
      "--camera-size=" + Math.round(root.nativeW) + "x" + Math.round(root.nativeH),
      "--camera-fps=30",
      "--v4l2-sink=" + root.node,
      "--no-audio", "--no-control",
      "--window-title=" + root.winTitle,
      "--window-width=640", "--window-height=360"]
    onExited: root.capturing = false
  }

  // Omarchy wraps Hyprland in Lua, so window rules go through `eval`, not
  // `keyword`. Applied before scrcpy's window maps, so it never flashes tiled.
  //
  // The theme has to come from RULES here, not from QML. When we drew the
  // preview ourselves we painted our own corners and border; scrcpy's window is
  // foreign, so the compositor is the only thing that can round or frame it.
  // `border_size = 0` was carried over from the QML version and is exactly why
  // the theming looked missing.
  Process {
    running: true
    command: ["bash", "-c",
      "hyprctl eval 'o.window({ title = \"^(omavcam preview)$\" }, " +
      "{ float = true, pin = true, no_dim = true, " +
      "rounding = " + Style.cornerRadius + ", " +
      "border_size = " + Style.normalBorderWidth + ", " +
      "opacity = \"1 1\", tag = \"-default-opacity\" })' >/dev/null"]
  }

  // scrcpy's window is a foreign toplevel, so unlike a window we own we have to
  // wait for it to appear before it can be placed. This is the cost ADR-0002
  // paid for mpv, and it is back — but it buys the free preview, and it is one
  // poll at startup rather than anything per frame.
  Timer {
    interval: 500
    running: !root.capturing
    repeat: true
    onTriggered: probe.running = true
  }

  Process {
    id: probe
    command: ["bash", "-c", "hyprctl clients -j | grep -c '\"omavcam preview\"' || true"]
    stdout: StdioCollector {
      onStreamFinished: {
        if (parseInt(text.trim()) > 0 && !root.capturing) {
          root.capturing = true
          // Size first, then anchor: scrcpy opens at its own --window-width,
          // which matches no preset, so the first resize would otherwise jump.
          root.pendingSize = true
          root.snap(0)
        }
      }
    }
  }

  // No window of our own. The old version needed one because it WAS the
  // preview; this one only issues commands, so a second window would be pure
  // clutter next to the thing you actually want to look at.
  //
  // Commands come in over IPC instead, which is also the shape the real plugin
  // has — a keybinding or the panel sends a request, it does not type into a
  // preview:
  //
  //   qs -p prototypes/preview ipc call preview anchor 3
  //   qs -p prototypes/preview ipc call preview size up
  //   qs -p prototypes/preview ipc call preview quit
  IpcHandler {
    target: "preview"
    function anchor(i: int): void { root.snap(Math.max(0, Math.min(8, i - 1))) }
    function size(dir: string): void { root.resize(dir === "down" ? -1 : 1) }
    function quit(): void { capture.running = false; Qt.quit() }
  }

  function previewSize() {
    const ar = nativeW / nativeH
    if (sizes[sizeIndex] === "original") return Qt.size(nativeW, nativeH)
    const h = Math.round((monitor ? monitor.h : 900)
                         * (sizes[sizeIndex] === "small" ? 0.18
                          : sizes[sizeIndex] === "medium" ? 0.28 : 0.40))
    return Qt.size(Math.round(h * ar), h)
  }

  function snap(i) { pendingAnchor = i; geom.running = true }
  function resize(d) {
    sizeIndex = Math.max(0, Math.min(sizes.length - 1, sizeIndex + d))
    pendingSize = true
    geom.running = true
  }

  // One read for both the monitor and scrcpy's current size. We cannot ask the
  // window how big it is the way we could when we owned it, so it is read back
  // from the compositor — on user actions only, never per frame.
  Process {
    id: geom
    command: ["bash", "-c", "hyprctl monitors -j; echo '@@'; hyprctl clients -j"]
    stdout: StdioCollector {
      onStreamFinished: {
        const parts = text.split("@@")
        if (parts.length < 2) return
        let m = null, w = null
        try {
          for (const d of JSON.parse(parts[0])) if (d.focused) m = d
          for (const c of JSON.parse(parts[1])) if (c.title === root.winTitle) w = c
        } catch (e) { return }
        if (!m || !w) return

        // hyprctl reports position and reserved in logical pixels but size in
        // physical ones, so the scale has to be divided out.
        const r = m.reserved
        root.monitor = {
          x: m.x + r[0], y: m.y + r[1],
          w: m.width / m.scale - r[0] - r[2],
          h: m.height / m.scale - r[1] - r[3]
        }

        // Sizing is `resize` with x/y meaning WIDTH and HEIGHT — the parameter
        // names lie, and `exact` is what stops them being read as a delta.
        if (root.pendingSize) {
          root.pendingSize = false
          const s = root.previewSize()
          Hyprland.dispatch('hl.dsp.window.resize({ window = "title:^(' + root.winTitle
                            + ')$", x = ' + s.width + ', y = ' + s.height + ', exact = true })')
          if (root.pendingAnchor < 0) { geom.running = true; return }
        }
        if (root.pendingAnchor < 0) return

        const i = root.pendingAnchor
        root.pendingAnchor = -1
        const g = Style.gapsOut
        const u = root.monitor
        const col = i % 3, row = Math.floor(i / 3)
        const x = col === 0 ? u.x + g
                : col === 1 ? u.x + (u.w - w.size[0]) / 2
                : u.x + u.w - w.size[0] - g
        const y = row === 0 ? u.y + g
                : row === 1 ? u.y + (u.h - w.size[1]) / 2
                : u.y + u.h - w.size[1] - g
        Hyprland.dispatch('hl.dsp.window.move({ window = "title:^(' + root.winTitle
                          + ')$", x = ' + Math.round(x) + ', y = ' + Math.round(y) + ' })')
      }
    }
  }
}
