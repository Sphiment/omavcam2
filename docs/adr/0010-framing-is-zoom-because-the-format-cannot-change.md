# Framing is zoom, because the output format cannot change under a consumer

The primary framing control is `--camera-zoom`, not `--crop`. The virtual
camera's frame size is chosen once and never changes while an application is
watching.

Measured on a Galaxy A54 (SM-A546E, Android 16), scrcpy 4.1, v4l2loopback
0.15.4, with `ffmpeg` reading the node as a stand-in consumer.

## The finding that forces this

**An application that opens the virtual camera pins its format. A writer that
then arrives with a different frame size delivers nothing — silently, and
forever.**

| What was done, consumer attached throughout | Result |
|---|---|
| Kill the writer | Consumer survives. Frames stop, frame counter freezes, no error. Caps revert to `Video Output` only *without* evicting the open consumer |
| Restart the writer at the **same** size | Consumer **resumes** (frame 427 → 444). A genuine brief blip |
| Restart the writer at a **different** size | Consumer **freezes permanently**. Node stays at the old 1280x720, no frames flow, and neither scrcpy, nor ffmpeg, nor the kernel reports anything |
| Restart at a different size with **nothing** attached | Works. The writer sets the format freely |

`keep_format` was `0` for all of these. The pinning comes from the open
consumer, not from that control.

The silence is what makes this bad. The failure mode is not "the application
drops the camera and the user re-picks it" — it is "the picture stops updating
and nothing anywhere says why".

**Crop changes the frame size**, so it is subject to all of the above: a
1280x720 capture with `--crop=640:360:100:100` produces a 640x360 node. Applying
a crop mid-call would freeze the call's camera permanently.

## Why zoom

`--camera-zoom=3` narrows the field of view and **the node stays 1280x720**.
Verified. No format change, so no freeze, so it is safe to apply while an
application is watching.

It is also what users actually mean. Cropping in Meet gives a tighter shot at
the same output resolution; `--crop` hands the application a physically smaller
image, which is not the same feature.

The limitation is that zoom is **centred**. It cannot express "shift the frame
left", which `--crop` can.

## Considered options for off-centre framing

**Relay through a second node.** scrcpy writes the full frame to a private node;
the daemon crops *and scales back to a fixed size*, then writes to the public
one. Verified working, and it delivers the thing the phone-side crop cannot:

> The crop rectangle was changed from `640:360:100:100` to `800:450:400:200`
> with a consumer attached, and the consumer **survived** — frames 632 → 1036,
> still alive, public node still 1280x720.

**Arbitrary, off-centre framing, adjustable live, mid-call.** The scaling is what
buys this, not the cropping: cropping alone still changes the output size and
still freezes consumers. Crop-then-scale-to-fixed is the whole trick.

Measured cost, steady state:

| Pipeline | scrcpy | relay | total |
|---|---|---|---|
| Crop on the phone | 36.4% | — | **~36%** |
| Full frame + host crop and scale | 50.5% | 26.9% | **~77%** |

Roughly 2.1x, and the full frame crosses the cable. Not the default for that
reason — most sessions never crop, and paying double for all of them to serve
some of them is the wrong trade.

**But it is enabled on demand**: when an off-centre crop is active, the daemon
runs the relay; otherwise the capture goes straight to the public node. The cost
is then paid only by the users of the feature, in the session where they use it.

**Refuse size-changing Apply while a consumer is attached.** Cheap and honest.
Still the rule for the one thing the relay cannot absorb — changing the *public*
frame size itself, which is by definition a format change.

## Consequences

**Three framing paths, in cost order**, and the daemon picks by what the user
asked for rather than making them choose:

| Framing | How | Cost | Live-adjustable |
|---|---|---|---|
| Centred, tighter | `--camera-zoom` | free | yes |
| Off-centre | full frame + host crop and scale | ~2.1x | yes |
| Off-centre, cheap | `--crop` on the phone | cheapest | **no** — freezes consumers |

Zoom is the default and covers the common case at no cost. Dragging the box
off-centre turns the relay on for that session. The phone-side crop remains the
cheapest way to hold a fixed off-centre frame, so it stays available for a crop
set *before* anything is watching — but it can never be adjusted live.

**The public node's frame size becomes a setting of its own**, decided when the
capture starts rather than derived from lens or crop. `--camera-size` still sets
it, and it still costs roughly 4x the pipeline CPU, but changing it is subject
to the same restriction as crop.

The relay is not dead — it stays the answer if off-centre framing during a call
ever becomes a requirement worth 25% CPU. It is deliberately not the answer now.

## What this means for holding the last frame

ADR-0007 promised the node stays open on the phone dropping out. It does, better
than expected: the consumer survives the writer's death untouched, and most
applications keep displaying their last decoded frame, which is the desired
behaviour with no work at all.

For applications that time out on stalled input, `timeout` delivers frames
regardless — verified: with `timeout=1000` set, the consumer kept receiving
frames after the writer was killed. `timeout_image_io` is a write-only button
that loads a specific image into that buffer, so a real last frame can be shown
rather than the default black.

**These are per-device V4L2 controls, not module parameters** — settable at
runtime with `v4l2-ctl -c`, by the ordinary user, with no root and without being
in the `video` group (the logind ACL grants it). The upstream README documents
them as module options; in 0.15.4 they are not. The module's actual parameters
are `debug, max_buffers, max_openers, devices, video_nr, card_label,
exclusive_caps, max_width, max_height`.

## Two nodes work

`devices=2 video_nr=42,43 card_label=omavcam,"omavcam studio" exclusive_caps=1,1`
creates both. Idle nodes advertise `Video Output` only and are invisible to
camera applications; the Studio node stayed output-only while the public node
was capturing. Lookup by `Card type` distinguishes them, so nothing needs a
hardcoded path.
