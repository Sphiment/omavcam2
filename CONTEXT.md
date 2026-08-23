# omavcam

Turns an Android phone into a webcam on Omarchy. A phone's camera or screen is
captured over adb, written to a virtual video device, and appears to Meet, Zoom,
OBS and Discord as an ordinary webcam.

## Language

### Capture

**Capture**:
One running stream from a phone into the virtual camera. Exactly one exists at a
time, and it is fixed at launch: changing anything about it means replacing it.
_Avoid_: stream, session, feed

**Source**:
What the capture is pointed at — the phone's `camera` or its `screen`. The two
share almost no settings.
_Avoid_: input, mode

**Lens**:
One of the phone's physical cameras, identified by the id
`scrcpy --list-cameras` reports. A phone has several, each with its own
resolutions and zoom range.
_Avoid_: camera (ambiguous with the phone as a whole), facing

**Virtual camera**:
The `v4l2loopback` node other applications open. It advertises itself as a
capture device only while a capture is writing to it.
_Avoid_: loopback, sink, /dev/video42, device

**Zoom**:
How tight the shot is, centred. Narrows the field of view without changing the
frame size, so it can be changed while an application is watching. Bounded by
the lens's reported zoom range. Not a crop, and never called one.
_Avoid_: crop, digital zoom, magnification

**Crop**:
The rectangle of the sensor that reaches the virtual camera, at any position.
Distinct from zoom because it can be off-centre. Comes in two modes, and the
user chooses:
- _on the phone_ — applied before encoding, so the discarded pixels never cross
  the cable. Cheapest, but changes the frame size, so it cannot be adjusted
  while an application is watching.
- _on the host_ — the full frame crosses the cable and is cropped and scaled
  back to a fixed size here. Roughly 2.1x the CPU, and adjustable live.

_Avoid_: zoom, framing, region

### Connection

**Phone**:
One Android device, identified by the serial adb reports. Several can be
attached at once — a second may simply be charging — so exactly one is
_selected_, and every adb and scrcpy command names it explicitly.
_Avoid_: device (ambiguous with the virtual camera), handset, target

**Connection**:
The adb link to the selected phone, established before any capture can start. It
is a phase with its own states, failures and guidance, not a precondition that
either holds or doesn't.
_Avoid_: device link, session

**Transport**:
How the connection is carried — `wired` over USB, or `wireless` over TCP after
pairing.
_Avoid_: connection type, method

**Pairing**:
The one-time exchange of a six-digit code that authorises wireless debugging.
Distinct from connecting, which happens every time and can fail on its own.
_Avoid_: authorising, trusting

### The engine

**Daemon**:
The one long-lived process that owns the connection, the capture and every
setting. Socket-activated, so it is started by being connected to and there is
no state in which it is "not running".
_Avoid_: server, backend, service (Omarchy's `service` plugin kind is a
different thing, and deliberately not this — see ADR-0001)

**Client**:
Anything holding a connection to the daemon: the CLI, the bar widget, Studio.
Clients render; they never own anything.
_Avoid_: consumer, subscriber, frontend

**State**:
The single object the daemon owns and pushes whole to every client whenever it
changes. There is one, it is complete, and a client that has it needs nothing
else to render.
_Avoid_: status, snapshot, model, data

**Revision**:
A counter that increases each time the state changes, carried on every pushed
state and on every response. It is what ties a response to the state that
reflects it. It counts *changes*, not requests, and is scoped to one run of the
daemon.
_Avoid_: version (taken by the protocol version), sequence, timestamp

**Request**:
One thing a client asks the daemon to do, named by its **kind** and carrying an
id the response echoes. Every request gets exactly one response, which either
succeeds or names a machine-readable error — that is where CLI exit codes come
from.
_Avoid_: command, call, action, message (a message is any line on the wire,
including a pushed state)

### Surfaces

**Studio**:
The full-screen control surface. Owns the connection flow, every setting, and
crop framing. Changes made here are pending until applied.
_Avoid_: settings window, editor, panel

**Panel**:
The popup behind the bar widget. A status light, a switch, and a way into
Studio. Never the place where settings are configured.
_Avoid_: widget, popup, menu

**Preview**:
A window showing what the capture is producing. Only one exists at a time: the
floating preview when Studio is closed, Studio's own when it is open.
_Avoid_: monitor, viewer, thumbnail

**Anchor**:
One of nine named positions the preview can be sent to, inset from the usable
area of a monitor. A destination you command, not a magnet you drag into.
_Avoid_: snap point, corner, position

**Apply**:
The act of committing pending settings, which replaces the running capture. The
virtual camera briefly disappears and returns.
_Avoid_: save, commit, confirm
