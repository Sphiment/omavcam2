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
One of the phone's physical cameras, identified by the id adb reports. A phone
has several, each with its own resolutions and zoom range.
_Avoid_: camera (ambiguous with the phone as a whole), facing

**Virtual camera**:
The `v4l2loopback` node other applications open. It advertises itself as a
capture device only while a capture is writing to it.
_Avoid_: loopback, sink, /dev/video42, device

**Crop**:
The rectangle of the sensor that reaches the virtual camera. Applied on the
phone before encoding, so it reduces what crosses the cable.
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
