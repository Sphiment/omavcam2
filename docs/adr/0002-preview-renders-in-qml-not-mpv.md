# The preview renders in QML, not mpv

Every existing V4L2 preview on this system shells out to mpv — Omarchy's own
webcam overlay does, and so did the previous omavcam. We render the virtual
camera directly in a QML `VideoOutput` instead.

## Why

mpv is a foreign process, so the old code had to poll `hyprctl clients` until
its window appeared before it could place it, drive drag detection through an
mpv Lua script and a generated `input.conf`, and parse `hyprctl monitors` for
geometry. A QML window is ours: it exists when we create it, we know its size,
and the theme applies to it. That deleted roughly 940 lines of shell, plus the
`mpv`, `jq` and `hyprctl` dependencies for the preview path.

Verified on real hardware before committing to it: a phone camera through
`scrcpy` into `/dev/video42` delivers ~30fps to a QML `VideoOutput` running
inside quickshell.

**Performance was not the reason, and does not favour either side.** Measured on
the same stream and window size: at 1280x720, mpv 5.4% CPU against QML 5.2% —
a tie. mpv only wins at the phone's native 3264x2448 (18.7% against 24.1%), and
that resolution is a mistake to stream regardless.

## Consequences

We are the only thing in the Omarchy ecosystem rendering video in QML, so
there is no prior art to copy when it breaks. Two undocumented traps were hit
within a day of starting; both are recorded in ADR-0006.

The virtual camera advertises one format only, fixed by whatever is writing to
it, so neither renderer can request a smaller capture. The only downscale lever
is scrcpy's `--camera-size` on the phone — worth roughly 4x the CPU of the whole
pipeline, far more than the renderer choice.
