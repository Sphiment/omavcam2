# Crop happens on the phone, and Studio previews uncropped

> **Superseded in part by ADR-0010.** Phone-side `--crop` remains the cheapest
> fixed off-centre path and the measurements below still stand. What this ADR
> missed is that crop changes the virtual camera's frame size. Live off-centre
> framing therefore uses an on-demand host crop *and scale back to the public
> size*; centred framing uses `--camera-zoom`. Read ADR-0010 first.

Phone-side cropping is applied by `scrcpy --crop` before encoding. Host-side
crop and scale is also offered when live adjustment is required.

## Why

`--crop=W:H:X:Y` works with `--video-source=camera`, not only screen mirroring —
verified on hardware: a 1280x720 capture with `--crop=640:360:100:100` produces a
640x360 virtual camera. So the device path takes arbitrary rectangles, and the
usual reason to prefer host-side cropping does not apply.

Measured, both producing identical 640x360 output:

| Where | CPU, whole capture chain | Over USB |
|---|---|---|
| On the phone (`--crop`) | 13.1% | 640x360 |
| On the laptop (scrcpy → fifo → ffmpeg → v4l2) | 25.4% | full 1280x720 |

Host-side costs roughly twice the CPU, sends the full frame across the cable to
throw half of it away, and puts another stage in the pipeline. Its advantage is
adjustment without restarting the capture. ADR-0010 later established that this
matters mid-call because a size-changing restart freezes an attached consumer,
so the cost is now paid on demand rather than rejected.

## Studio must preview the uncropped frame

**A crop cannot be re-framed if only its inside is visible.** Growing the box
back outward requires seeing what is being cut off, so Studio shows the whole
sensor with the crop rectangle drawn over it. The floating preview and the
virtual camera show the result.

This has a consequence that looks like a UI rule but is a hard constraint: since
the crop is applied on the phone, the uncropped pixels never cross the cable, so
**Studio cannot show the full frame while a crop is active**. Therefore:

- opening Studio replaces the capture with an uncropped one,
- Apply replaces it again with the crop, and Studio closes,
- the two previews can never be alive at once — they need different scrcpy
  invocations, not merely different windows.

## Consequences

Every setting scrcpy fixes at launch requires replacing that direct capture.
Studio batches those changes behind Apply. The host relay is the exception: its
crop rectangle is a live variable and its output is scaled back to the fixed
public size, so changing it neither restarts scrcpy nor changes the public
format.
