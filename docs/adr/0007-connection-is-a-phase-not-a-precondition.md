# Connection is a phase, not a precondition

Getting a phone attached is modelled as a stage with its own states, failures and
guidance — not as a check that either passes or fails before the real work.

## Why

The previous version treated devices as a list to pick from: either adb reported
one or it didn't. That is adequate over USB, where the only failure is an
unaccepted debugging prompt. It falls apart for wireless, where a user can fail
at pairing, at connecting, at being on the wrong network, or by having the phone
asleep — each needing different advice.

**This adb cannot discover devices** (ADR-0006: no mDNS), so a wireless
connection is the user reading an address and a six-digit code off their phone
and typing them in. The instructions are the feature.

States worth naming, each mapping to something adb reports or a step that can
visibly fail: no phone · several attached, none selected · selected but
unauthorised · needs pairing · pairing failed · paired, connecting ·
unreachable · connected · lost, reconnecting.

## One phone is selected, and every command names it

`adb devices` can list several. The common case is not two phones a user wants
to choose between — it is one phone they use and one charging off the laptop,
which appears as `unauthorized` and would be picked by anything that takes the
first entry.

So selection is part of this machine, not a step before it, and **every `adb`
and `scrcpy` invocation is targeted with `-s <serial>`**. Untargeted commands
are correct exactly until a second phone is plugged in, which makes them a
latent bug that testing on a tidy desk will never surface.

The selected phone is remembered and re-selected when it reappears. When it is
absent and another phone is attached, omavcam **does not** switch to it: silently
repointing a webcam at a different room is worse than reporting no phone.

Settings are keyed per phone. Lens ids, available resolutions and zoom ranges
are device-specific, so a lens id from one phone is meaningless on another.

## Wireless means pairing, not `adb tcpip`

Two wireless paths exist. `adb tcpip 5555` followed by `adb connect` needs a
cable once and does not survive a phone reboot. Android 11+ wireless debugging
uses `adb pair HOST:PORT CODE` and needs no cable ever.

Only pairing is supported. Working without a cable and surviving reboots is the
entire point of wireless; `adb tcpip` is a shortcut for someone already holding a
cable, and can be added if anyone asks.

## Where it lives

Studio owns the flow, because guides and error states do not belong in a bar
popup. The panel shows connection state and a button that opens Studio at the
right place. The panel stays a status light and a switch.

**Amended (#6): plus the picker, while nothing is chosen.** Choosing between
two attached phones is the one step of this machine that is a choice rather
than a guide — there is nothing to explain and nothing that can fail, only two
names and a click. Sending someone to Studio to click one of two buttons costs
more than it teaches, and the panel is where they already are. It stays out of
the panel in every other state: once a phone is remembered there is no choice
left to make, and repointing the webcam at the other phone is a deliberate act
that stops a running capture, which belongs with the rest of the flow.

## Losing the phone mid-capture

Treated as a state of this same machine rather than an error path of its own: the
virtual camera node is held open showing the last frame, the daemon reconnects
when the phone returns, and the capture resumes. Releasing the node instead would
make applications drop the camera permanently — the difference between a hiccup
and a dead webcam mid-meeting.

**Verified, and it costs nothing.** A consumer survives the writer being killed:
it stays open, frames simply stop, and no error is raised — so applications go on
showing their last decoded frame by themselves. The capability flags revert to
output-only without evicting the consumer that is still attached. For anything
that times out on stalled input, the `timeout` control keeps frames flowing; see
ADR-0010. The one hard constraint is that the capture must resume at the **same
frame size**, or the consumer freezes permanently instead of resuming.
