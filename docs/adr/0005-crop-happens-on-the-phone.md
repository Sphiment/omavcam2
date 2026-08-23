# Crop happens on the phone, and Studio previews uncropped

Cropping is applied by `scrcpy --crop` on the device, before encoding. Host-side
cropping is not offered.

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
throw half of it away, and puts ffmpeg and a fifo in the pipeline. Its only
advantage is adjustment without restarting the capture — worth nothing here,
because settings are applied in a batch anyway.

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

Every setting scrcpy fixes at launch — lens, resolution, frame rate, crop —
requires replacing the capture, during which the virtual camera disappears and
returns. Applications usually reacquire it, but it is a visible blip. Studio
therefore batches changes behind an Apply button: one blip per session rather
than one per adjustment.
