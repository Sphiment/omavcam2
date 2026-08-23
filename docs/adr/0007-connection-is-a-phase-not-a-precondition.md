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
visibly fail: no device · found but unauthorised · needs pairing · pairing
failed · paired, connecting · unreachable · connected · lost, reconnecting.

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

## Losing the phone mid-capture

Treated as a state of this same machine rather than an error path of its own: the
virtual camera node is held open showing the last frame, the daemon reconnects
when the phone returns, and the capture resumes. Releasing the node instead would
make applications drop the camera permanently — the difference between a hiccup
and a dead webcam mid-meeting.
