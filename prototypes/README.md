# Prototypes

Throwaway code kept for reference, not shipped and not maintained. It exists
because the decisions in `docs/adr/` were reached by building and measuring
these, and the working shape is easier to read than to reconstruct.

## `preview/shell.qml`

The floating preview as the ADRs describe it: a `FloatingWindow` rendering
`/dev/video42` through a QML `VideoOutput`, themed from Omarchy's own tokens,
with the nine anchors as commands and **no drag handling at all** — Hyprland's
`general:snap` does that (ADR-0004).

Worth reading for the three things that are not obvious (all in ADR-0006): the
`String()` around `videoInputs[].id`, the rebind timer that exists because the
node only enumerates while something writes to it, and the Lua form of the
Hyprland dispatch.

Run it standalone. `qs.Ui` and `qs.Commons` resolve relative to the config root,
so they need linking in first — the real plugin, loaded by the shell, gets them
without this:

```bash
ln -s /usr/share/omarchy/shell/Ui      prototypes/preview/Ui
ln -s /usr/share/omarchy/shell/Commons prototypes/preview/Commons
qs -p prototypes/preview
```

Do not copy those symlinks into an installed plugin: `omarchy-plugin-validate`
refuses any symlink inside a plugin folder.

Keys: `1`-`9` anchor (reading order, 1 = top-left) · `+`/`-` size · `esc` quit.
Move it with Super+drag like any other window.

## `phone`

Starts a real capture from a plugged-in phone, the way the daemon will:

```bash
./prototypes/phone start [camera-id] [fps] [WxH]   # e.g. start 1 30 1280x720
./prototypes/phone cams
./prototypes/phone stop
```

Always pass a size. The phone's default is 3264x2448, which costs ~19-24% of a
core to decode; 1280x720 costs ~5% and is what every meeting app downscales to
anyway (ADR-0002).
