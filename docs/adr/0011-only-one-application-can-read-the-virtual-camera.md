# Only one application can read a camera at a time, and that is not our doing

A V4L2 camera serves exactly one streaming client. A second one is refused with
`EBUSY`. This is true of the virtual camera, and equally true of the laptop's
real webcam.

## The measurement

With one `ffmpeg` streaming from the virtual camera, a second reader fails:

```
$ ffmpeg -f v4l2 -i /dev/video42 ...
Error opening input: Device or resource busy

$ v4l2-ctl -d /dev/video42 --stream-mmap --stream-count=5
VIDIOC_REQBUFS returned -1 (Device or resource busy)
```

It is not an ffmpeg quirk — `v4l2-ctl` fails identically, and it fails at
`VIDIOC_REQBUFS`, after `open()` has already succeeded. `max_openers` governs
`open()`, not streaming.

Two hypotheses were tested and both were wrong:

- **Buffer starvation.** Reloaded with `max_buffers=16` instead of the default
  2. No change.
- **Something omavcam configured.** The control test settles it: the **real HP
  laptop webcam refuses a second reader too**, with the same error. Nothing
  about the virtual camera is special here.

So this is ordinary Linux camera behaviour, and every user already lives with
it. It is not a defect introduced by this design.

## Why it still matters

**The floating preview and a video call both want to read the same node.** So
the arrangement the preview exists for — a call running, the preview open beside
it to check framing — is the one that hits this.

This is exactly the problem **PipeWire** solves: it opens the device once and
fans frames out to as many clients as ask. It is why a browser and a preview can
coexist on a normal desktop at all.

And here is the connection that makes it our problem after all:

**`exclusive_caps=1` hides the virtual camera from PipeWire.** With a capture
running, `wpctl status` listed `omavcam` and `omavcam studio` as *Devices* but
created no `Video/Source` node for either — only the HP webcam had one. The node
advertises output-only capability until a producer attaches, WirePlumber probes
it while it is idle, sees no capture capability, and never revisits.

So the setting chosen to keep omavcam out of camera dropdowns when idle also
opts us out of the mechanism that would let the preview and a call coexist.

## The trade

| `exclusive_caps` | Idle behaviour | Sharing |
|---|---|---|
| `1` | Absent from camera dropdowns until capturing | No PipeWire source; one reader only |
| `0` | Always present in dropdowns, showing nothing when idle | PipeWire can adopt it and fan out |

Neither is obviously right. Always appearing in every application's camera list,
showing black when idle, is the thing `exclusive_caps=1` was chosen to avoid.

## PipeWire was tested, and it did not work

`exclusive_caps=0` does produce the source node. PipeWire then describes it
correctly — `pw-cli enum-params` reports `I420`, `1280x720`, the live phone
stream, with the node `suspended` while no client is attached.

**But no PipeWire client was ever able to stream from it.** Every attempt failed
at `Failed to set pipeline to PAUSED` or `stream error: target not found`, while
`fuser` showed PipeWire never opening `/dev/video42` at all — only scrcpy, the
writer, was there. The same clients stream the laptop webcam without trouble.

A likely explanation, untested: PipeWire's V4L2 source negotiates with
`VIDIOC_S_FMT`, and a loopback node with an attached producer will not accept a
format change. It would fit ADR-0010's finding that the format is fixed by
whoever is already there.

### Three false positives, and what caught them

Early runs appeared to show two clients streaming concurrently. They were all
wrong: `gst-launch pipewiresrc` **silently falls back to the default camera**
when its target cannot be resolved, so those tests were measuring the laptop
webcam. The node ids churn — omavcam moved through `57`, `67`, `73` and serials
`79`, `84`, `87` within one session — so any id captured a moment earlier was
already stale.

What caught it was checking `fuser /dev/video0` *during* each run and rejecting
the result if the webcam had been touched. Any future test in this area must do
the same; a green result without an attribution check means nothing here.

## Status

**Undecided, and the PipeWire option is now the weakest of the three.** It
cannot be relied on without first explaining and fixing why PipeWire cannot open
a loopback node that a plain `ffmpeg` reader opens without complaint.

That leaves feeding the preview outside V4L2 — the daemon already holds the
frames whenever it is relaying — as the option that survives every case.

It gates ADR-0002 and ADR-0003 and should be settled before the preview is
built.
