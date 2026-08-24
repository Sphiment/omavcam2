# omavcam

Turns an Android phone into a webcam on Omarchy. A phone's camera or screen is
captured over adb, written to a virtual video device, and appears to Meet, Zoom,
OBS and Discord as an ordinary webcam.

This is the rewrite. The shape is a Rust **daemon** that owns everything, and
thin **clients** — a CLI, a bar widget, and Studio — that render what it pushes
them. See [CONTEXT.md](CONTEXT.md) for the vocabulary and
[docs/adr/](docs/adr/) for why it is built this way.

## Status

Early. What exists today is the daemon, the socket protocol, the test harness
everything else is built on, wired connection — plug a phone in over USB and
`omavcam status` names it — and a capture that can be started and stopped:
`omavcam start` and the phone's camera appears in Meet's camera list. There is
also a bar widget, so none of that needs a terminal.

There is no wireless or settings yet — the capture runs at its defaults. Those
are the next tickets.

## Requirements

- Rust (`pacman -S rustup && rustup default stable`)
- systemd (user session)
- `android-tools` for `adb`, `scrcpy` for the capture, and `v4l-utils` for
  `v4l2-ctl` — none of them needed to build or test, only to run
- `v4l2loopback-dkms`, loaded and labelled `omavcam` at boot. That is the
  package's job, not the daemon's: a `systemd --user` service has no capability
  to load a module (ADR-0008). Until [#16](../../issues/16) packages it:

  ```sh
  echo v4l2loopback | sudo tee /etc/modules-load.d/omavcam.conf
  echo 'options v4l2loopback video_nr=42 card_label=omavcam exclusive_caps=1' \
    | sudo tee /etc/modprobe.d/omavcam.conf
  ```

  `exclusive_caps=1` is what keeps an idle omavcam out of every application's
  camera list, and what makes the node readable at all (ADR-0012).

## Build

```sh
cargo build --release
```

The binary lands at `target/release/omavcam`.

## Install

Nothing packages this yet — that is [#16](../../issues/16). Until then, by hand:

```sh
sudo install -Dm755 target/release/omavcam /usr/bin/omavcam
install -Dm644 systemd/omavcam.socket ~/.config/systemd/user/omavcam.socket
install -Dm644 systemd/omavcam.service ~/.config/systemd/user/omavcam.service
systemctl --user daemon-reload
systemctl --user enable --now omavcam.socket
```

The binary has to be at `/usr/bin/omavcam` because that is the path
`omavcam.service` names.

**Enabling the socket is what makes the daemon exist.** systemd holds the
listening socket and starts the daemon the first time something connects to it,
which is why there is no "daemon isn't running" error in this product. It also
means the daemon survives `omarchy-restart-shell`: it is a systemd unit, not an
Omarchy `service` plugin, and those are QML singletons that die with the shell
(ADR-0001).

## Use

```sh
omavcam status            # print the daemon's state
omavcam select <serial>   # choose which attached phone to use (bare: lists them)
omavcam start             # start the capture: the phone's camera becomes a webcam
omavcam stop              # end it; omavcam disappears from camera lists again
omavcam refresh           # re-check adb and the attached phones now
omavcam daemon            # run the daemon in the foreground (systemd's job, normally)
```

Exit codes: `0` the request succeeded, `1` the daemon refused it and said why on
stderr, `2` the daemon could not be reached at all.

## Which phone

The daemon asks `adb devices -l` once a second, so plugging a phone in or
unplugging it is noticed without anyone running a command.

One phone attached is selected automatically. Several and omavcam asks, because
the second phone on a desk is usually one that is charging — and it shows up as
`unauthorized`, so anything taking the first entry from `adb devices` points the
webcam at a phone in someone's pocket. The choice is remembered in
`~/.local/state/omavcam/phones.json` and re-made for you whenever that phone is
attached. When it is *not* attached, omavcam reports no phone rather than
switching to whatever else is plugged in: a webcam should not change rooms
without being asked. That holds even when the other phone is the only one on
the desk, so a phone you have never used before is one `omavcam select` away —
run it bare and it lists what is attached.

Changing the selected phone stops a running capture first. A capture is bound
to the phone it was launched against; changing the status label while leaving
the old phone on camera would be both misleading and a privacy failure.

Every `adb` call that addresses a phone names it with `-s <serial>`. The only
two that do not are `start-server` and `devices`, which have no phone to name.
A test asserts this over the recorded argv, because an untargeted command is
correct right up until a second phone is plugged in.

See [ADR-0007](docs/adr/0007-connection-is-a-phase-not-a-precondition.md).

Logs go to the journal:

```sh
journalctl --user -u omavcam.service -f
```

## The capture

`omavcam start` launches one `scrcpy` against the selected phone, writing its
camera straight to the virtual camera at 1280x720. `omavcam stop` ends it, and
omavcam then disappears from every application's camera list — that is the
point of `exclusive_caps=1`, not a side effect: an idle omavcam showing black
in Meet's dropdown is what it avoids. The corollary is that omavcam is absent
from the list until a capture is running, so it is picked after starting.

Locking the phone mid-capture is fine: the stream is unaffected, and both
lenses capture with the screen off and the lockscreen up. **Face unlock is
not.** The recognition service takes a camera to do its job, and the limit on
open cameras is system-wide, so unlocking that way while a capture is running
takes the camera out from under it — unlock with a PIN instead, or start the
capture afterwards.

scrcpy dying on its own — the phone unplugged, the process killed — moves the
state to stopped, so the switch never claims to be on while nothing is feeding.
An application already watching survives that: frames stop, it keeps showing its
last one, and a restart at the **same** frame size resumes. A restart at a
different size would freeze it permanently and silently, which is why the size
is fixed for a capture's lifetime and why there is no resolution setting yet
(ADR-0010).

The daemon leaves `keep_format=0`: an application already reading the camera
pins the format by itself, while an idle node must remain free to accept the
size changes ticket #9 allows. `sustain_framerate` and `timeout` keep consumers
that dislike stalled input attached across a same-size restart.

The same scrcpy process also draws the floating preview. It is launched with a
known title and `--no-control`, so clicks and typing are never forwarded to the
phone. The panel hides it by moving that window off-screen and restores its last
position; scrcpy and the virtual camera keep running throughout. Hyprland rules
make it floating, pinned on every workspace, aspect-ratio preserving,
unfocusable, and resistant to the compositor's close shortcut. They also apply
the active Omarchy corner radius and border width.

`omavcam start --stay-awake` is refused while the floating preview is part of
the capture. scrcpy itself forbids `--stay-awake` together with the mandatory
`--no-control`; omavcam does not weaken input safety or take ownership of
restoring the phone's power setting.

## The bar widget

An Omarchy plugin lives at the root of this repo, so it installs the ordinary
way and there is nothing to build:

```sh
omarchy plugin add https://github.com/Sphiment/omavcam2.git --enable
```

The icon says which of three things is true at a glance: a capture is running,
nothing is capturing, or something is wrong that only the person at the desk
can fix — adb missing, the daemon unreachable, a phone that has not accepted
the debugging prompt. Finding that out before the call is the entire point.

Clicking it opens the **panel**: a status light, the connection in words, and
switches for the capture and its preview. Whenever more than one phone is
attached the panel offers them and picking one takes effect — whatever phase
the connection is in, because the second phone appearing on the desk is the
moment someone wants to switch. The phone in use is marked, one that has not
accepted the debugging prompt is dimmed and said to be, and switching while a
capture is running says it will stop the capture before it does. Nothing else
goes in there — settings are Studio's job, and the frequent action has to be
instant.

The plugin holds no state and makes no system calls. It opens the socket,
renders what it is pushed, and sends requests; every `scrcpy`, `adb` and
`hyprctl` invocation in this project lives in the daemon (ADR-0001), and a
test asserts the plugin has no way to run one. Connecting is also what starts
the daemon, so the widget appearing in the bar is what makes omavcam exist.

State changed from elsewhere — `omavcam start` in a terminal, a phone
unplugged — arrives as a push, so the panel is right without being reopened.
A daemon that stops leaves the widget saying so and reconnecting recovers it,
which is the whole of the recovery: the daemon pushes its state on connect and
there is nothing to resync. The wait between attempts doubles up to half a
minute, because a daemon that cannot start at all would otherwise be asked to
twice a second forever, and every attempt is another failed activation.

## How the pieces talk

Line-delimited JSON over a unix socket in the runtime dir. The daemon pushes the
**whole** state to every connected client whenever it changes, and once on
connect. Nothing polls, and a client that reconnects after a daemon restart is
correct immediately with no resync request.

Alongside that, every request carries an id which its response echoes, plus an
explicit success or machine-readable error — that is where the CLI's exit codes
come from. Messages carry a protocol version, and requests are bounded at 64 KiB.
External commands have a ten-second deadline, so a wedged adb cannot freeze the
connection machine or every client behind it.

The full shape is documented at the top of [`src/protocol.rs`](src/protocol.rs),
and the reasoning is in
[ADR-0014](docs/adr/0014-the-daemon-pushes-whole-state-and-answers-every-request.md).

## Tests

```sh
cargo test
```

Tests spawn the **real** daemon against a temp state dir, with a directory of
stub `adb`, `scrcpy`, `v4l2-ctl` and `modprobe` executables ahead of the real
ones on `PATH`, and a fake `/sys/class/video4linux` to find the virtual camera
in. Each stub records the argv it was called with, prints whatever the test
scripted for it, and stays running while the test holds it — which is how a
capture that is still going, or one that dies on its own, is expressed.

The stub directory *is* the fake — there is no process-runner abstraction inside
the daemon to swap out, so the tests exercise the same code that runs in
production.

That harness is in [`tests/common/mod.rs`](tests/common/mod.rs) and the stub
itself is [`tests/stub`](tests/stub). One test drives real socket activation
through `systemd-socket-activate`, so the activation path is covered without
installing anything.

## Layout

```
src/protocol.rs   the wire format, shared by the daemon and the CLI
src/command.rs    concrete subprocess deadlines; PATH remains the test seam
src/daemon.rs     the daemon: state, clients, pushing
src/phones.rs     what adb sees, which phone is selected, what that means
src/capture.rs    finding the virtual camera, and launching scrcpy at it
src/main.rs       the CLI, and the entry point for both
manifest.json     the Omarchy plugin manifest, at the root so a clone installs
plugin/Panel.qml  the bar widget and its panel: a client, and nothing more
systemd/          the socket and service units
tests/common/     the harness every test file is built on
tests/            the tests, and the stub adb/scrcpy/v4l2-ctl/modprobe
CONTEXT.md        the vocabulary; use these words, avoid the listed synonyms
docs/adr/         why things are the way they are
docs/agents/      how agents should work in this repo
```
