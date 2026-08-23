# Platform traps that are not visible in the code

Constraints of Omarchy, Hyprland, Qt and adb that each cost real debugging time
and are documented nowhere upstream. Recorded so they are found by reading
rather than by rediscovery.

## Omarchy wraps Hyprland in Lua, so `hyprctl dispatch` does not work

```
$ hyprctl dispatch exec true
error: [string "return hl.dispatch(exec true)"]:1: ')' expected near 'true'
```

Every dispatch must be written as Lua, and `hyprctl keyword` is refused outright
("keyword can't work with non-legacy parsers. Use eval."):

```
hyprctl dispatch 'hl.dsp.window.move({ window = "title:^(x)$", x = 10, y = 20 })'
hyprctl eval     'o.window({ title = "^(x)$" }, { float = true, pin = true })'
hyprctl eval     'hl.config({ general = { gaps_out = 10 } })'
```

**Quickshell's `Hyprland.dispatch()` is therefore unusable in its documented
form** — it wraps the argument in `hl.dispatch(...)`, so it must be handed Lua.
The previous omavcam carried the same workaround in `hypr_dispatch()`.

## `videoInputs[].id` is a QByteArray, not a string

```qml
if (String(devices.videoInputs[i].id) === "/dev/video42")   // String() is load-bearing
```

Without `String()`, `===` against a string literal is silently false, no error is
raised, and QtMultimedia falls back to the *default* camera — the laptop webcam.
The preview looks perfectly fine while showing the wrong device.

## The virtual camera only enumerates while something writes to it

`exclusive_caps=1` means the node advertises capture capability only while a
capture is running. That is deliberate — it keeps "omavcam" out of every
application's camera dropdown when idle — but it means the device is absent from
`MediaDevices.videoInputs` until the stream starts. Binding must be a retry, not
a one-shot at startup.

It also advertises exactly one format, fixed by the writer, so no reader can
request a smaller capture. `--camera-size` on the phone is the only downscale
lever, and it is worth roughly 4x the CPU of the entire pipeline.

## Hyprland emits no geometry events

Subscribing to `.socket2.sock` while moving and resizing windows produces
nothing, on a socket where `openwindow`, `closewindow`, `activewindow`,
`activelayout` and `windowtitle` all fire normally. There is no move, resize or
drag-end event. See ADR-0004 for what this rules out.

## This adb has no mDNS

```
$ adb mdns services
adb: mdns is not supported by this version of adb.
```

`android-tools 37.0.0` on Arch is built without it, so wireless devices cannot be
discovered. The user must read an address and pairing code off the phone and type
them in, which is why the connection flow's guidance is a feature and not
decoration.

## `Style.gapsOut` is not `gaps_out`

Omarchy's theme token reads 5 while Hyprland's `general:gaps_out` is 10 — it
mirrors `gaps_in`. Anything insetting the preview to match where a tiled window
sits must read the compositor, not the token.
