# Uncropped frames never reach the public virtual camera

**Invariant: whatever a crop hides must never be visible to an application
consuming the virtual camera, at any moment, including while Studio is open.**

## Why

ADR-0005 established that Studio previews the uncropped sensor, because a crop
whose outside you cannot see cannot be re-framed outward. It also established
that the crop is applied on the phone, so showing the uncropped frame means
launching a different capture.

Both are still right. What was missed is where those uncropped frames go.

If the uncropped capture writes to the same node applications consume, then
opening Studio to adjust framing **during a call** sends everyone the whole
sensor — precisely the thing the user cropped away. The feature exists to hide
the rest of the room, and the act of adjusting it would reveal the rest of the
room. That is a privacy failure, not a rough edge.

The mid-call case is not exotic. Adjusting framing is exactly what someone does
when a colleague says "you're a bit off centre".

## Consequences

The uncropped capture needs a destination that is not the public node, and the
public node needs to keep satisfying its consumer while that happens — an
application that loses the device mid-call does not quietly get it back.

The leading design is **two nodes**: a public one that applications see, and a
Studio one they are never told about, with the public node holding a frame while
Studio has the phone. Since ADR-0008 moved module configuration to install time,
the node count is a packaging decision and must be made before the daemon is
written, not after.

**This is not settled, because the mechanism it depends on is unverified.**
v4l2loopback's `timeout_image` shows a *timeout picture, a null frame by
default* — not the last real frame — so holding the last frame requires
something to capture and write it. Whether that works, whether a second writer
can hold a node open, and whether consumers survive it are hardware questions.
See the pipeline spike ticket; this ADR fixes the invariant, not the
implementation.

A weaker rule — stop the public capture entirely while Studio is open — also
satisfies the invariant and is the fallback if holding a frame proves
impractical. It costs the consumer losing the device, which is worse but honest.
