# Prototypes

**Throwaway code. Not the implementation, not a template, not maintained.**

It exists because the decisions in `docs/adr/` were reached by building and
measuring these, and the working shape is easier to read than to reconstruct
from prose. Read it to see that the shape works. Then write the real thing
properly — do not port this file and clean it up afterwards.

## What must not survive into the real plugin

Every one of these is a deliberate shortcut taken to answer a question faster,
and every one is wrong for a shipped plugin:

- **It talks to the system directly.** `hyprctl` through `Process`, `scrcpy`
  through a shell script. In the real thing the daemon owns every system call
  and the QML only speaks to its socket (ADR-0001). A plugin shelling out is
  exactly the structure the rewrite exists to remove.
- **It polls.** A one-second timer retries binding to the virtual camera. The
  daemon pushes state instead, so nothing in the UI should have a poll loop.
- **It hardcodes what the daemon owns.** The device path, camera ids, frame
  rate, and the size presets `0.18 / 0.28 / 0.40`. These are settings.
- **It has no error handling.** No reconnect, no disconnect state, no reporting
  when `scrcpy` dies — the whole of ADR-0007 is missing.
- **It persists nothing.** Anchor and size reset on every launch.
- **It pins to one monitor for sizing** and does not follow the preview when it
  is moved to another.
- **Its keyboard handling is a hack.** `forceActiveFocus()` plus click-to-refocus
  exists so the number keys work while testing. The real anchors come from the
  panel and the CLI, and the preview should not take focus at all.
- **It uses `Style.gapsOut`, which is wrong.** That token reads 5 while
  Hyprland's `gaps_out` is 10 — it mirrors `gaps_in` (ADR-0006).

## What is worth carrying over

The shape, and the three things that are easy to get wrong and cost real time to
find — all detailed in ADR-0006 and visible here in context:

- `String()` around `videoInputs[].id`, without which the wrong camera binds
  silently;
- rebinding as a retry, because the node only enumerates while something writes
  to it;
- the Lua form of the Hyprland dispatch.

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
