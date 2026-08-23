# The floating preview is scrcpy's own window

One `scrcpy` process renders the preview window **and** writes the virtual
camera, from a single decode. No relay, no second node, no frame copying.

```
scrcpy --video-source=camera --v4l2-sink=/dev/video42 --no-control \
       --window-title="omavcam preview" ...
```

## Why

ADR-0011 concluded the daemon had to fan out to a second node, because a V4L2
camera serves only one streaming client and the preview would be competing with
the video call. That is still true of *reading* the node — but it turns out the
preview never needed to read it.

scrcpy already decodes every frame in order to write the sink. Drawing them in
its own window costs almost nothing more, because the drawing is OpenGL and
happens on the GPU.

Measured at 720p30:

| Approach | CPU |
|---|---|
| scrcpy, no window (baseline) | 18.2-23.1% |
| **scrcpy, window + v4l2 sink** | **24.0%** |
| daemon fan-out to 2 nodes, both read | 32.2% |

Run-to-run noise is about ±5 points, so the window is free or nearly so — and
the fan-out is roughly 10 points more expensive for the same result, while also
needing a relay, an extra node, and extra entries in every application's camera
list (ADR-0012 rules out hiding them).

Verified on hardware: with the window rendering, `ffmpeg` read the node
concurrently without trouble, and scrcpy stayed up.

## It is a real window, so everything else still applies

Hyprland reports it as an ordinary floating toplevel:

```
title='omavcam preview'  class='scrcpy'  size=[480,270]  floating=True
```

So the reasoning in ADR-0003 and ADR-0004 carries over unchanged: alt-tab
reaches it, Omarchy's Super+drag moves it, the compositor's own snapping applies,
and the nine anchors work as commands against `title:^(omavcam preview)$`.
Window rules by class or title supply rounding and borders.

`--no-control` is required, or the window forwards clicks and keystrokes to the
phone.

## Toggling it without disturbing the capture

The preview cannot be closed and reopened, because the window belongs to the
capture process — closing it kills scrcpy and takes the webcam with it,
mid-call. Hiding is a compositor operation instead. Verified: moving the window
off-screen and back leaves the node feeding throughout.

```
hl.dsp.window.move({ window = "title:^(omavcam preview)$", x = -2000, y = -2000 })
```

**The window must therefore be made unclosable, or closing it must be caught and
turned into a hide.** A user clicking the X on their preview must not lose their
camera in a meeting.

## Consequences

ADR-0011's fan-out is not needed for the floating preview, and neither is its
relay. Where the relay survives is off-centre crop (ADR-0010), which needs the
daemon between scrcpy and the public node for its own reasons.

**This does not extend to Studio.** Studio draws a crop rectangle over the
video, which requires the frames inside our own UI, so Studio still renders in
QML from a node. That is consistent: ADR-0005 already has Studio replacing the
capture and closing the floating preview, so the two never contend for a reader.

ADR-0002 chose QML over mpv for the floating preview. That comparison is
superseded here — the winner is neither, because the process already decoding
the frames can simply show them.
