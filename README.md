# vcamd

Turns an Android phone into a webcam on Omarchy. A phone's camera or screen is
captured over adb, written to a virtual video device, and appears to Meet, Zoom,
OBS and Discord as an ordinary webcam.

This repository is the Rust **daemon** and its CLI. The daemon owns everything;
thin clients render the state it pushes them. The Omarchy wrapper lives in the
separate [`Sphiment/omavcamd`](https://github.com/Sphiment/omavcamd) repository.
See [CONTEXT.md](CONTEXT.md) for the vocabulary and
[docs/adr/](docs/adr/) for why it is built this way.

## Status

Early. What exists today is the daemon, the socket protocol, the test harness
everything else is built on, wired and wireless connection, and a capture that
can be started and stopped: `vcamd start` makes the phone's camera appear in
Meet's camera list. Lens, resolution, frame rate, aspect ratio and zoom can be
staged and applied from the CLI. An Arch package — built by CI and installed
from this repo's releases — installs the engine and configures the module, so
none of it has to be assembled by hand.

There is no Studio yet. That is the next ticket.

## Install

Install this engine first, then install the separate Omarchy wrapper if you want
bar controls. The daemon and CLI work without the wrapper.

```sh
# 0. Up to date, and able to build a kernel module. v4l2loopback is a DKMS
#    module, and the kernel headers it builds against are only an *optional*
#    dependency of dkms — pacman will not pull them in, and without them the
#    module is never built. Match the kernel you actually run:
#    linux-lts-headers, linux-zen-headers, linux-hardened-headers.
sudo pacman -Syu linux-headers

# 1. The engine, prebuilt by this repo's CI.
sudo pacman -U https://github.com/Sphiment/vcamd/releases/latest/download/vcamd-git-x86_64.pkg.tar.zst

# 2. Reboot. The package configures the module to load at boot and enables the
#    daemon's socket for every user, and neither is picked up by a session that
#    was already running. To skip the reboot, do both by hand instead:
sudo modprobe v4l2loopback
systemctl --user daemon-reload && systemctl --user start vcamd.socket

# 3. Optional: the Omarchy bar wrapper.
omarchy plugin add https://github.com/Sphiment/omavcamd.git --enable
```

`omarchy plugin add --enable` asks where in the bar to put the widget, so that
step is interactive.

On the phone, turn on **Developer options** — tap Build number seven times in
Settings → About phone — and inside it turn on **USB debugging**. Then plug the
phone in, accept the debugging prompt it shows, and click the widget, or run
`vcamd start`. The virtual camera appears in Meet, Zoom, OBS and Discord once
a capture is running, and disappears again when it stops.

If `modprobe v4l2loopback` says there is no such module, the DKMS build did not
run: `dkms status` shows whether it built for the kernel you are on, and the
usual cause is step 0's headers being absent or for a different kernel.

After the reboot nothing here needs root again: connecting to the socket is
what starts the daemon, and the module is already loaded and labelled.

That URL always points at the newest release, and pacman pulls in the
dependencies from its own repositories. To upgrade later, run the same command
again — the package is not in a pacman repository, so `pacman -Syu` will not
find a new one for you. `pacman -Q vcamd-git` says which version you have.

x86_64 only: that is what the CI builds. On aarch64, build it yourself — see
below, it is the same PKGBUILD.

The package pulls in everything vcamd needs: `scrcpy`, `android-tools` for
`adb`, `v4l-utils` for `v4l2-ctl`, `v4l2loopback-dkms` for the virtual camera
itself, and `hyprland` for the `hyprctl` every capture uses to place its
preview. If one of them goes missing later, `vcamd status` and the
panel name it and the package that supplies it rather than failing at the
moment you need the camera.

### What the package installs, and why root is only involved once

| Path | What it is |
|---|---|
| `/usr/bin/vcamd` | the daemon and the CLI, one binary |
| `/usr/lib/systemd/user/vcamd.{socket,service}` | socket activation: connecting is what starts the daemon |
| `/usr/lib/systemd/user/sockets.target.wants/vcamd.socket` | enabled on install, for every user |
| `/usr/lib/modules-load.d/vcamd.conf` | loads `v4l2loopback` at boot |
| `/usr/lib/modprobe.d/vcamd.conf` | its parameters: two nodes, labelled, `exclusive_caps=1` |

The module configuration is installed rather than applied at runtime because a
`systemd --user` service has **no capabilities at all** — measured `CapEff:
0000000000000000`, so `modprobe` fails for it (ADR-0008). Nothing in vcamd
ever calls `modprobe`; a test asserts it.

That means the module is loaded from boot and its nodes always exist. It is
harmless: with `exclusive_caps=1` a node advertises output only until a
producer attaches, so an idle vcamd is in nobody's camera list.

`exclusive_caps=1` is on **every** node, including the internal one, and it is
not optional. A node created with `exclusive_caps=0` advertises capture and
output at once, which makes browsers refuse to list it *and* makes it
unreadable — the single cause behind three separate failures (ADR-0012). Two
nodes are created: the public `vcamd`, and `vcamd studio` for Studio's
uncropped preview, which must never reach the node applications consume
(ADR-0009). To change any of this, put your own file in `/etc/modprobe.d` — it
wins over the package's.

That cuts both ways: if you set vcamd up by hand before there was a package,
delete the files you wrote or they will keep overriding it, and you will have
one node where the package creates two.

```sh
sudo rm -f /etc/modprobe.d/vcamd.conf /etc/modules-load.d/vcamd.conf
sudo modprobe -r v4l2loopback && sudo modprobe v4l2loopback
```

### Building it yourself

Working on vcamd, or on an architecture the CI does not build:

```sh
git clone https://github.com/Sphiment/vcamd.git
cd vcamd
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
sudo install -Dm755 target/release/vcamd /usr/bin/vcamd
sudo install -Dm644 systemd/vcamd.socket /usr/lib/systemd/user/vcamd.socket
sudo install -Dm644 systemd/vcamd.service /usr/lib/systemd/user/vcamd.service
sudo install -Dm644 packaging/vcamd.modules-load.conf /usr/lib/modules-load.d/vcamd.conf
sudo install -Dm644 packaging/vcamd.modprobe.conf /usr/lib/modprobe.d/vcamd.conf
systemctl --user enable --now vcamd.socket
sudo modprobe v4l2loopback
```

The binary has to be at `/usr/bin/vcamd`, because that is the path
`vcamd.service` names.

### Where the package comes from

`.github/workflows/package.yml` builds the PKGBUILD in an Arch container on
every push, so a broken package is a failed check rather than a surprise at
install time. Pushing a `v*` tag publishes that build as a release asset under
a fixed name, which is what the install command above downloads.

```sh
git tag v0.1.0 && git push origin v0.1.0   # cuts a release
```

It is not an AUR package: the AUR is closed to new submissions at the moment.
The PKGBUILD is written so it can become one unchanged — CI overrides only
where it clones from, so that a released package is the tagged commit rather
than whatever the default branch had drifted to.

### Uninstall

```sh
systemctl --user stop vcamd.socket vcamd.service
sudo pacman -Rns vcamd-git  # takes the units and the module configuration with it
omarchy plugin remove sphiment.omavcamd
```

`stop`, not `disable`: the socket was enabled by a symlink the package owns, so
removing the package is what disables it, and `vcamd.service` is never
enabled at all — it exists to be socket-activated.

Removing the package unloads `v4l2loopback` as well, so nothing is left holding
a camera node. A module something else is actually using has a non-zero
refcount and is left alone — and nothing vcamd installed will load it again.

## Suggested: make the preview snap where you want it

This is a change **you** make, not one vcamd makes. Hyprland's own snapping
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

vcamd will never write this for you. It is a global compositor setting that
changes how *every* floating window behaves, and a plugin has no business
deciding that. vcamd writes nothing to your Hyprland config at all.

## Platform scope

The daemon, CLI, and capture need systemd, Hyprland, and the tools above. The
preview's window rules use Omarchy's Lua wrapper (`hl.config`, `o.window`), so
this repository is still tested on Omarchy rather than stock Hyprland. The
Omarchy shell integration itself belongs to the wrapper repository.

## Use

```sh
vcamd status            # print the daemon's state
vcamd phones            # list remembered wired and wireless phones
vcamd select <serial>   # choose which attached phone to use (bare: lists them)
vcamd pair              # show Android's wireless-pairing steps
vcamd pair <pair-address> <code> <connect-address>
vcamd connect <serial> <new-connect-address>  # update a changed connect port
vcamd forget <serial>   # remove a remembered phone
vcamd start             # start the capture: the phone's camera becomes a webcam
vcamd stop              # end it; vcamd disappears from camera lists again
vcamd set lens 1        # stage one or several camera settings
vcamd set resolution 1920x1080
vcamd set frame-rate 24
vcamd set aspect-ratio 16:9
vcamd set zoom 2.5
vcamd set crop 0.1:0.1:0.8:0.8  # normalized x:y:width:height; "none" clears it
vcamd apply             # replace a capture once, or just persist if stopped
vcamd discard           # drop every staged change
vcamd refresh           # re-check adb and the attached phones now
vcamd daemon            # run the daemon in the foreground (systemd's job, normally)
```

Exit codes: `0` the request succeeded, `1` the daemon refused it and said why on
stderr, `2` the daemon could not be reached at all.

## Which phone

The daemon asks `adb devices -l` once a second, so plugging a phone in or
unplugging it is noticed without anyone running a command.

One phone attached is selected automatically. Several and vcamd asks, because
the second phone on a desk is usually one that is charging — and it shows up as
`unauthorized`, so anything taking the first entry from `adb devices` points the
webcam at a phone in someone's pocket. The choice is remembered in
`~/.local/state/vcamd/phones.json` and re-made for you whenever that phone is
attached. When it is *not* attached, vcamd reports no phone rather than
switching to whatever else is plugged in: a webcam should not change rooms
without being asked. That holds even when the other phone is the only one on
the desk, so a phone you have never used before is one `vcamd select` away —
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

Run `vcamd pair` for the steps. On the phone, open Developer options →
Wireless debugging, then open **Pair device with pairing code**. That dialog's
address and six-digit code are for pairing. The address on the main Wireless
debugging screen is for connecting. The ports are different; vcamd asks for
both so it cannot silently use one for the other:

```sh
vcamd pair 192.168.1.40:37123 123456 192.168.1.40:42877
```

Pairing is one-time. Later daemon starts use only `adb connect`; no cable and
no `adb tcpip 5555` are involved. If Android changes the connect port after a
reboot or Wireless debugging toggle, copy the new main-screen address and run:

```sh
vcamd connect <old-connect-address> <new-connect-address>
```

Do not pair again. An unreachable paired phone reports that state separately
and names being on different networks, a sleeping phone, and a changed connect
port as the things to check. `vcamd phones` lists remembered phones by model;
`vcamd forget <serial>` removes one.

Logs go to the journal:

```sh
journalctl --user -u vcamd.service -f
```

## The capture

`vcamd start` launches one `scrcpy` against the selected phone, writing its
camera straight to the virtual camera with that phone's applied settings.
Capabilities come from `scrcpy --list-camera-sizes`, so each lens exposes its
own resolutions, frame rates and zoom bounds rather than a hardcoded menu.
Settings are remembered per phone. `vcamd stop` ends the capture, and
vcamd then disappears from every application's camera list — that is the
point of `exclusive_caps=1`, not a side effect: an idle vcamd showing black
in Meet's dropdown is what it avoids. The corollary is that vcamd is absent
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
never focused on its own, and resistant to the compositor's close shortcut.
It stays draggable with Omarchy's Super+drag, onto a second monitor included;
it is deliberately not made unfocusable, because that takes it out of the
compositor's reach and the drag with it (ADR-0013). They also apply
the active Omarchy corner radius and border width.

`vcamd start --stay-awake` is refused while the floating preview is part of
the capture. scrcpy itself forbids `--stay-awake` together with the mandatory
`--no-control`; vcamd does not weaken input safety or take ownership of
restoring the phone's power setting.

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
systemd/          the socket and service units
packaging/        the PKGBUILD, its install hook, and the module config
.github/          the workflow that builds the package and publishes releases
tests/common/     the harness every test file is built on
tests/            the tests, and the stub adb/scrcpy/v4l2-ctl/modprobe
CONTEXT.md        the vocabulary; use these words, avoid the listed synonyms
docs/adr/         why things are the way they are
docs/agents/      how agents should work in this repo
```
