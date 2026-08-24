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
everything else is built on, wired and wireless connection, and a capture that
can be started and stopped: `omavcam start` makes the phone's camera appear in
Meet's camera list. Lens, resolution, frame rate, aspect ratio and zoom can be
staged and applied from the CLI. There is also a bar widget, so starting and
stopping needs no terminal, and an AUR package that installs the engine and
configures the module, so none of it has to be assembled by hand.

There is no Studio yet. That is the next ticket.

## Install

omavcam is two halves: the **engine** — a Rust daemon and the `omavcam` CLI,
installed from the AUR — and the **bar widget**, an Omarchy plugin installed by
cloning this repo. Both are needed: the widget is a client and does nothing on
its own.

```sh
# 1. The engine. Building takes a few minutes; it is Rust.
yay -S omavcam-git

# 2. Reboot. The package configures the module to load at boot and enables the
#    daemon's socket for every user, and neither is picked up by a session that
#    was already running. To skip the reboot, do both by hand instead:
sudo modprobe v4l2loopback
systemctl --user daemon-reload && systemctl --user start omavcam.socket

# 3. The bar widget.
omarchy plugin add https://github.com/Sphiment/omavcam2.git --enable
```

Then plug a phone in, accept the debugging prompt on it, and click the widget —
or run `omavcam start`. The virtual camera appears in Meet, Zoom, OBS and
Discord once a capture is running, and disappears again when it stops.

After the reboot nothing here needs root again: connecting to the socket is
what starts the daemon, and the module is already loaded and labelled.

The package pulls in everything omavcam needs: `scrcpy`, `android-tools` for
`adb`, `v4l-utils` for `v4l2-ctl`, `v4l2loopback-dkms` for the virtual camera
itself, and `hyprland` for the `hyprctl` every capture uses to place its
preview. If one of them goes missing later, `omavcam status` and the
panel name it and the package that supplies it rather than failing at the
moment you need the camera.

### What the package installs, and why root is only involved once

| Path | What it is |
|---|---|
| `/usr/bin/omavcam` | the daemon and the CLI, one binary |
| `/usr/lib/systemd/user/omavcam.{socket,service}` | socket activation: connecting is what starts the daemon |
| `/usr/lib/systemd/user/sockets.target.wants/omavcam.socket` | enabled on install, for every user |
| `/usr/lib/modules-load.d/omavcam.conf` | loads `v4l2loopback` at boot |
| `/usr/lib/modprobe.d/omavcam.conf` | its parameters: two nodes, labelled, `exclusive_caps=1` |

The module configuration is installed rather than applied at runtime because a
`systemd --user` service has **no capabilities at all** — measured `CapEff:
0000000000000000`, so `modprobe` fails for it (ADR-0008). Nothing in omavcam
ever calls `modprobe`; a test asserts it.

That means the module is loaded from boot and its nodes always exist. It is
harmless: with `exclusive_caps=1` a node advertises output only until a
producer attaches, so an idle omavcam is in nobody's camera list.

`exclusive_caps=1` is on **every** node, including the internal one, and it is
not optional. A node created with `exclusive_caps=0` advertises capture and
output at once, which makes browsers refuse to list it *and* makes it
unreadable — the single cause behind three separate failures (ADR-0012). Two
nodes are created: the public `omavcam`, and `omavcam studio` for Studio's
uncropped preview, which must never reach the node applications consume
(ADR-0009). To change any of this, put your own file in `/etc/modprobe.d` — it
wins over the package's.

That cuts both ways: if you set omavcam up by hand before there was a package,
delete the files you wrote or they will keep overriding it, and you will have
one node where the package creates two.

```sh
sudo rm -f /etc/modprobe.d/omavcam.conf /etc/modules-load.d/omavcam.conf
sudo modprobe -r v4l2loopback && sudo modprobe v4l2loopback
```

### Building it yourself

No AUR helper, or working on omavcam:

```sh
git clone https://github.com/Sphiment/omavcam2.git
cd omavcam2
cargo build --release   # needs rustup: pacman -S rustup && rustup default stable
```

Or build the package itself. makepkg insists on being run from the directory
its PKGBUILD is in, and it clones the repo from GitHub rather than packaging
your working tree:

```sh
cd packaging && makepkg -si
```

By hand, which is the same set of files without pacman knowing about them:

```sh
sudo install -Dm755 target/release/omavcam /usr/bin/omavcam
sudo install -Dm644 systemd/omavcam.socket /usr/lib/systemd/user/omavcam.socket
sudo install -Dm644 systemd/omavcam.service /usr/lib/systemd/user/omavcam.service
sudo install -Dm644 packaging/omavcam.modules-load.conf /usr/lib/modules-load.d/omavcam.conf
sudo install -Dm644 packaging/omavcam.modprobe.conf /usr/lib/modprobe.d/omavcam.conf
systemctl --user enable --now omavcam.socket
sudo modprobe v4l2loopback
```

The binary has to be at `/usr/bin/omavcam`, because that is the path
`omavcam.service` names.

### Uninstall

```sh
systemctl --user stop omavcam.socket omavcam.service
yay -Rns omavcam-git          # takes the units and the module configuration with it
omarchy plugin remove sphiment.omavcam2
```

`stop`, not `disable`: the socket was enabled by a symlink the package owns, so
removing the package is what disables it, and `omavcam.service` is never
enabled at all — it exists to be socket-activated.

Removing the package unloads `v4l2loopback` as well, so nothing is left holding
a camera node. A module something else is actually using has a non-zero
refcount and is left alone — and nothing omavcam installed will load it again.

## Suggested: make the preview snap where you want it

This is a change **you** make, not one omavcam makes. Hyprland's own snapping
is what parks the floating preview neatly (ADR-0004), and `respect_gaps` is
what parks it at `gaps_out` instead of flush against the screen edge:

```lua
hl.config({ general = { snap = {
  enabled = true, respect_gaps = true, monitor_gap = 60, window_gap = 60,
} } })
```

`monitor_gap` and `window_gap` are **thresholds** — how close you have to drag
before the magnet grabs — not the resulting inset. The default of 10 makes them
nearly impossible to hit; 60–90 feels right.

omavcam will never write this for you. It is a global compositor setting that
changes how *every* floating window behaves, and a plugin has no business
deciding that. omavcam writes nothing to your Hyprland config at all.

## This is Omarchy-specific

The widget is an Omarchy plugin and assumes Omarchy's shell: its `Panel`,
`Toggle` and `Style` components, its theme tokens, and `omarchy plugin add` as
the way it is installed. The preview's window rules are written in Omarchy's
Lua wrapper (`hl.config`, `o.window`), which stock Hyprland does not have.

The engine itself is less fussy — the daemon, the CLI and the capture need only
systemd, Hyprland and the tools above — but nothing here is tested anywhere
else, and the Lua assumptions do not hold on stock Hyprland.

## Use

```sh
omavcam status            # print the daemon's state
omavcam phones            # list remembered wired and wireless phones
omavcam select <serial>   # choose which attached phone to use (bare: lists them)
omavcam pair              # show Android's wireless-pairing steps
omavcam pair <pair-address> <code> <connect-address>
omavcam connect <serial> <new-connect-address>  # update a changed connect port
omavcam forget <serial>   # remove a remembered phone
omavcam start             # start the capture: the phone's camera becomes a webcam
omavcam stop              # end it; omavcam disappears from camera lists again
omavcam set lens 1        # stage one or several camera settings
omavcam set resolution 1920x1080
omavcam set frame-rate 24
omavcam set aspect-ratio 16:9
omavcam set zoom 2.5
omavcam set crop 0.1:0.1:0.8:0.8  # normalized x:y:width:height; "none" clears it
omavcam apply             # replace a capture once, or just persist if stopped
omavcam discard           # drop every staged change
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

## Wireless pairing

Run `omavcam pair` for the steps. On the phone, open Developer options →
Wireless debugging, then open **Pair device with pairing code**. That dialog's
address and six-digit code are for pairing. The address on the main Wireless
debugging screen is for connecting. The ports are different; omavcam asks for
both so it cannot silently use one for the other:

```sh
omavcam pair 192.168.1.40:37123 123456 192.168.1.40:42877
```

Pairing is one-time. Later daemon starts use only `adb connect`; no cable and
no `adb tcpip 5555` are involved. If Android changes the connect port after a
reboot or Wireless debugging toggle, copy the new main-screen address and run:

```sh
omavcam connect <old-connect-address> <new-connect-address>
```

Do not pair again. An unreachable paired phone reports that state separately
and names being on different networks, a sleeping phone, and a changed connect
port as the things to check. `omavcam phones` lists remembered phones by model;
`omavcam forget <serial>` removes one.

Logs go to the journal:

```sh
journalctl --user -u omavcam.service -f
```

## The capture

`omavcam start` launches one `scrcpy` against the selected phone, writing its
camera straight to the virtual camera with that phone's applied settings.
Capabilities come from `scrcpy --list-camera-sizes`, so each lens exposes its
own resolutions, frame rates and zoom bounds rather than a hardcoded menu.
Settings are remembered per phone. `omavcam stop` ends the capture, and
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

If scrcpy dies, the logical capture moves to `Reconnecting` and keeps the same
phone and applied settings. An application already watching survives that:
frames stop, it keeps showing its last one, and the daemon resumes at the
**same** frame size when the phone answers again. A different size would freeze
the consumer permanently and silently, so Apply refuses resolution or
phone-side crop changes while another application has the virtual camera open.
Same-size changes restart safely. If the new capture fails, Apply relaunches
the previous settings and reports what was rejected (ADR-0010).

The daemon leaves `keep_format=0`: an application already reading the camera
pins the format by itself, while an idle node must remain free to accept the
size changes ticket #9 allows. `sustain_framerate` repeats the last real frame
while the writer reconnects; `timeout` stays off because its default is black.

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

An Omarchy plugin lives at the root of this repo, so the widget itself installs
the ordinary way:

```sh
omarchy plugin add https://github.com/Sphiment/omavcam2.git --enable
```

The widget is only a client, though: the engine has to be installed too, or
the bar shows a widget with nothing behind it — see [Install](#install). When
that is the case the panel says so and names the command that fixes it, and it
does the same for a missing `scrcpy`, `adb` or `v4l2loopback-dkms`: the daemon
reports what it cannot find and the package that supplies it, and the panel
offers the install. It never runs one — it runs nothing at all (ADR-0001).

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
packaging/        the AUR PKGBUILD, its install hook, and the module config
tests/common/     the harness every test file is built on
tests/            the tests, and the stub adb/scrcpy/v4l2-ctl/modprobe
CONTEXT.md        the vocabulary; use these words, avoid the listed synonyms
docs/adr/         why things are the way they are
docs/agents/      how agents should work in this repo
```
