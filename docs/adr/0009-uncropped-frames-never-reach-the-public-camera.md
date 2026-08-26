# Uncropped frames never reach the public virtual camera

> **Mechanism settled by ADR-0010, ADR-0012 and ADR-0013.** Two nodes are
> packaged: public `vcamd` and `vcamd-studio` — hyphenated, because a
> `card_label` in a `modprobe.d` file cannot hold a space. The kernel's
> `next_arg()` strips quotes only when the whole value begins with one, and
> `param_array_set` then splits on `,` with no quote handling, so a quoted
> label arrives with its quote characters attached. #17 measured this on a
> shell command line, where the shell had already removed them.
>
> The public writer stops while
> Studio owns the phone; an already-open consumer survives and holds its last
> decoded frame. scrcpy writes the uncropped feed only to the Studio node.

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

The design is **two nodes**: a public one applications consume, and a Studio one
used only for the uncropped preview. Since ADR-0008 moved module configuration
to install time, both are created and labelled by the package.

The #17 spike established that an open consumer survives the public writer's
death without being evicted. Most applications keep showing their last decoded
frame; `timeout` can keep frames arriving for consumers that dislike a stall.
The public node therefore needs no second writer while Studio has the phone.

ADR-0012 establishes the discoverability cost plainly: an internal V4L2 node
cannot stay hidden while a producer is attached. The Studio node is absent from
camera lists while idle and appears, under an honest internal-purpose label,
while Studio is open. Any requirement that it never appear while active would
contradict the measured V4L2 capability behaviour and needs a different
transport, not another node flag.
