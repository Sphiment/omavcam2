# Rust engine behind a long-lived daemon

The previous version was a 2346-line bash CLI that every UI shelled out to.
Nothing was running between invocations, so each command re-derived the world by
scanning pids, parsing `hyprctl` output and reading state files. With a bar
widget, Studio and a CLI all driving one capture, that approach gets worse, not
better. The engine is a Rust daemon that owns scrcpy, adb, the `v4l2loopback`
module, and every setting; the surfaces are thin clients.

## Considered options

**Stay in bash.** The work is mostly invoking `scrcpy`, `adb` and `modprobe`,
which is bash's home turf, and it was already written. Rejected because a socket
server that pushes state changes to several clients is genuinely awkward in
bash, and because the 63 `jq` invocations in the old CLI were a symptom of
serving structured state from a language that has none.

**Go.** Faster to write for this shape of program — process supervision, JSON,
sockets — and builds in seconds. Chosen first, then reversed in favour of Rust
on the author's preference.

## Consequences

The daemon is socket-activated by systemd rather than always running, so it
costs nothing when no phone is attached and there is no "daemon isn't running"
error state: connecting starts it.

It must survive the shell dying. Omarchy has a `service` plugin kind, but those
are QML singletons that die with the shell, and `omarchy-restart-shell` is
something users run. Capture state living there would mean a shell restart kills
the webcam mid-meeting.

**Distribution is the real cost of compiling at all.** Omarchy installs plugins
by `git clone` with no build step, so the binary cannot ship inside the plugin.
It goes to the AUR, and the plugin becomes a thin QML shim that offers to
install it — the same flow the old version already used for `scrcpy` and
`v4l2loopback-dkms`. An AUR `-git` package means users compile on their own
machines, and Rust takes minutes where Go took seconds. Accepted deliberately.
