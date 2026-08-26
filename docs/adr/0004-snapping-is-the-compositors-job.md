# Snapping is the compositor's job

vcamd implements no drag-snapping. Hyprland's built-in `general:snap` does it.
The nine anchors survive only as commands — `vcamd preview move top-right`,
the panel, a keybinding — which are exact and need no drag at all.

This ADR exists because the obvious reading of the old codebase is "snapping was
hard, so they wrote a lot of code", and the next person will be tempted to write
it again. Don't. Five approaches were built and measured first.

## Why implementing it is a trap

**Wayland tells a client neither where it is nor when a drag ended.** The second
half was verified rather than assumed: subscribing to Hyprland's event socket
while moving and resizing windows programmatically produced *nothing*, on a
socket where `openwindow`, `closewindow`, `activewindow` and `windowtitle` all
fire normally. There is no drag-end event to listen for. Every approach below is
an attempt to manufacture that missing signal.

| Approach | Outcome |
|---|---|
| Old version: mpv Lua script reports mouse-up, plus a watcher loop | Worked. Cost most of ~940 lines of shell, and only possible because mpv was doing the rendering |
| Move the window ourselves each mouse-move, tracking our own position | Thrashed. We can't know our real position, so every event re-applied a delta that hadn't landed — overshoot, correct, oscillate |
| `startSystemMove()`, then detect the end from the window going still | Snapped ~300ms after release from a stillness guess. Laggy, and snapped mid-drag if the user paused |
| Drive the drag ourselves with `hyprctl cursorpos` as the reference | Correct — the cursor doesn't depend on our position, so no feedback loop, and we keep the pointer grab so the release is exact. Cost a `hyprctl` fork 60x/second for the duration of every drag |
| `startSystemMove()`, treat the pointer re-entering our surface as the end | Failed. The window moves *under* a stationary cursor, so the enter event fires at the drag's start. It snapped once, mid-hold |

## The answer

```lua
hl.config({ general = { snap = {
  enabled = true, respect_gaps = true, monitor_gap = 10, window_gap = 10,
} } })
```

The compositor is running the drag, so it is the only thing that knows when the
drag ended and where the window is. Nothing has to be manufactured.

**The previous version's README stated that Hyprland's snap "parks windows flush
against the edge, with no way to inset that line".** That is out of date:
`respect_gaps` parks at `gaps_out` instead. Believing it cost four attempts.

Note that `monitor_gap` and `window_gap` are *thresholds* — how close you must
drag before it grabs — not the resulting inset. The default of 10 makes the
magnet hard to hit; 60-90 feels closer to the old behaviour.

## Consequences

This is a **global compositor setting**, affecting every floating window. It
belongs in the user's `~/.config/hypr/looknfeel.lua`, suggested in the README.
vcamd must not switch it on silently: a plugin has no business changing how
all of someone's windows behave.

Hyprland snaps to monitor edges and other windows, not to nine named positions —
there is no centre-of-screen magnet. The named anchors remain available as
commands, which is where they were always more useful.
