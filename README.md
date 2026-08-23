# omavcam

Turns an Android phone into a webcam on Omarchy. A phone's camera or screen is
captured over adb, written to a virtual video device, and appears to Meet, Zoom,
OBS and Discord as an ordinary webcam.

This is the rewrite. The shape is a Rust **daemon** that owns everything, and
thin **clients** — a CLI, a bar widget, and Studio — that render what it pushes
them. See [CONTEXT.md](CONTEXT.md) for the vocabulary and
[docs/adr/](docs/adr/) for why it is built this way.

## Status

Early. What exists today is the walking skeleton: the daemon, the socket
protocol, and the test harness everything else is built on.

`omavcam status` starts the daemon and prints its state. There is no phone
handling and no capture yet — those are the next tickets.

## Requirements

- Rust (`pacman -S rustup && rustup default stable`)
- systemd (user session)
- `android-tools` for `adb` — not needed to build or test, only to run

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
omavcam status     # print the daemon's state
omavcam refresh    # re-check the tools the daemon depends on
omavcam daemon     # run the daemon in the foreground (systemd's job, normally)
```

Exit codes: `0` the request succeeded, `1` the daemon refused it and said why on
stderr, `2` the daemon could not be reached at all.

Logs go to the journal:

```sh
journalctl --user -u omavcam.service -f
```

## How the pieces talk

Line-delimited JSON over a unix socket in the runtime dir. The daemon pushes the
**whole** state to every connected client whenever it changes, and once on
connect. Nothing polls, and a client that reconnects after a daemon restart is
correct immediately with no resync request.

Alongside that, every request carries an id which its response echoes, plus an
explicit success or machine-readable error — that is where the CLI's exit codes
come from. Messages carry a protocol version, and requests are bounded at 64 KiB.

The full shape is documented at the top of [`src/protocol.rs`](src/protocol.rs),
and the reasoning is in
[ADR-0014](docs/adr/0014-the-daemon-pushes-whole-state-and-answers-every-request.md).

## Tests

```sh
cargo test
```

Tests spawn the **real** daemon against a temp state dir, with a directory of
stub `adb`, `scrcpy` and `modprobe` executables ahead of the real ones on
`PATH`. Each stub records the argv it was called with and prints whatever the
test scripted for it. The stub directory *is* the fake — there is no
process-runner abstraction inside the daemon to swap out, so the tests exercise
the same code that runs in production.

That harness is in [`tests/skeleton.rs`](tests/skeleton.rs) and the stub itself
is [`tests/stub`](tests/stub). One test drives real socket activation through
`systemd-socket-activate`, so the activation path is covered without installing
anything.

## Layout

```
src/protocol.rs   the wire format, shared by the daemon and the CLI
src/daemon.rs     the daemon: state, clients, pushing
src/main.rs       the CLI, and the entry point for both
systemd/          the socket and service units
tests/            the harness
CONTEXT.md        the vocabulary; use these words, avoid the listed synonyms
docs/adr/         why things are the way they are
docs/agents/      how agents should work in this repo
```
