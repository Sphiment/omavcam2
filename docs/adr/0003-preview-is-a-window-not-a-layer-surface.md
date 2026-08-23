# The preview is a real window, not a layer surface

The floating preview is a `FloatingWindow` — an ordinary xdg-toplevel — rather
than a `wlr-layer-shell` surface of the kind Omarchy's bar and overlays use.

## Why

A layer surface is tempting and was prototyped first. It is handed the usable
area of its output automatically (`ExclusionMode.Normal` gives 1600x874 where
the full screen is 1600x900, and it resizes itself when the bar moves), so bar
clearance needs no arithmetic at all. It also never fights the compositor for
position.

It was rejected on one fact: **a layer surface belongs to exactly one output and
cannot span two.** It is created for a specific `wl_output` by protocol. On a
multi-monitor desktop the preview could teleport between screens at best, and
never straddle the gap. The previous version handled this correctly — its README
promised "drag it onto a second monitor and it snaps to that monitor's edges" —
so choosing layer shell would have been a regression.

A real window also behaves like a window: alt-tab reaches it, Omarchy's
universal Super+drag moves it, and window rules apply.

## Consequences

The usable area has to be asked for rather than given: `hyprctl monitors -j`
reports each monitor's `reserved` insets. Note the unit mismatch — position and
`reserved` are logical pixels, `width`/`height` are physical, so the monitor's
`scale` must be divided out.

Wayland never tells a client its own position, so anything that needs the
preview's coordinates must query the compositor. This is cheap because it
happens on user actions, never per frame — see ADR-0004 for why nothing needs
it continuously.
