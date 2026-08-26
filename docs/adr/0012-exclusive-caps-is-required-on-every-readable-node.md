# `exclusive_caps=1` is required on every node anything reads

A `v4l2loopback` node created with `exclusive_caps=0` is **write-only in
practice**. A producer can write to it; nothing can read it while that producer
is attached.

## The measurement

```
scrcpy writing 720p to /dev/video45   (exclusive_caps=0)

$ ffmpeg -f v4l2 -i /dev/video45
ioctl(VIDIOC_STREAMON): Input/output error
Error opening input: Input/output error
```

The cause is visible in the capabilities. With `exclusive_caps=0` a node reports
**both** at once:

```
video42 (exclusive_caps=1, idle)   Video Output
video45 (exclusive_caps=0, idle)   Video Capture  Video Output
```

`exclusive_caps=1` makes the node advertise output-only until a producer
attaches and capture-only afterwards. Without it the node never resolves into a
capture device, and readers fail at `STREAMON`.

## It explains three separate dead ends

Each of these was investigated as its own problem before the common cause was
found:

- **Browsers would not list vcamd.** Chromium filters devices advertising both
  capture and output. With `exclusive_caps=0` the camera vanished from Meet's
  device list entirely — verified on hardware.
- **PipeWire created a source node but never streamed from it.** It described
  the node correctly (`I420`, `1280x720`) and then no client could pull a frame,
  while `fuser` showed PipeWire never opening the device. Same `STREAMON` wall.
- **The fan-out relay could not read its own source node.** Same error, same
  cause.

The catch that follows: PipeWire *only* saw vcamd when `exclusive_caps=0`, and
that is exactly the setting that makes browsers hide it and readers fail. There
is no configuration where the PipeWire route works. ADR-0011's PipeWire option
is closed.

## Consequences

**Every node in the pipeline must be `exclusive_caps=1`**, including internal
ones the user is never meant to pick.

There is therefore **no way to hide an internal node**. Any node with a producer
attached announces itself as a camera and appears in every application's device
list. A fan-out that is live will show its extra nodes to the user, so they need
names that make their purpose obvious, and they should only be written while
actually in use.
