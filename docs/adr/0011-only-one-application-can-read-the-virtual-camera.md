# Only one application can read the virtual camera at a time

`v4l2loopback` permits many *openers* but only one *streamer*. A second client
that opens the node and asks for buffers is refused.

## The measurement

With one `ffmpeg` streaming from the node, a second reader fails:

```
$ ffmpeg -f v4l2 -i /dev/video42 ...
Error opening input: Device or resource busy

$ v4l2-ctl -d /dev/video42 --stream-mmap --stream-count=5
VIDIOC_REQBUFS returned -1 (Device or resource busy)
```

It is not an ffmpeg quirk — `v4l2-ctl` fails the same way, and it fails at
`VIDIOC_REQBUFS`, after `open()` has already succeeded. `max_openers=10` governs
`open()`, not concurrent streaming. A writer and a reader coexist fine; two
readers do not.

## Why this matters more than it looks

**The floating preview reads the same node the video call reads.** So the
headline arrangement — a call running, the preview open beside it so you can see
your framing — is the exact case that cannot work. Whichever opens second gets
`EBUSY`.

It is worse than a missing feature: it means opening the preview during a call
could take the camera away from the call, or fail in a way the user reads as
omavcam being broken.

Nothing in ADR-0002 or ADR-0003 anticipated this. Both discuss *how* the preview
renders, and neither asks whether it is allowed to.

## Options

**A third node and a fan-out.** The relay writes every frame to the public node
and to a preview node; the preview always reads its own. Costs the relay running
in every session, including ones with no crop — the ~2.1x that ADR-0010
deliberately made conditional.

**Feed the preview outside V4L2.** The daemon already has the frames when it is
relaying; it could hand them to the QML side directly over the existing socket
or shared memory, and the preview would never touch a video node. No extra node,
no extra copy to the kernel, and it works regardless of who holds the camera.
More code, and the QML side stops being a plain `VideoOutput`.

**Preview yields.** The preview reads the public node when nothing else is, and
shows "in use by another application" when something is. Free, honest, and gives
up the feature exactly when it is most wanted.

## Status

**Undecided.** The measurement is solid; the response is not, and it is a
big enough change to the preview's architecture that it should be chosen
deliberately rather than defaulted into. It gates ADR-0002 and ADR-0003, and it
should be settled before the preview ticket is built.
