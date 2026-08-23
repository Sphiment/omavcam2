# The module is loaded at install time, not by the daemon

`v4l2loopback` is configured by files the package installs — `modules-load.d` to
load it and `modprobe.d` to set its parameters. The daemon never calls
`modprobe`. It looks the node up and reports if it is missing.

## Why

**A `systemd --user` daemon has no capabilities at all.** Measured on this
machine:

```
$ grep CapEff /proc/self/status
CapEff:	0000000000000000
$ modprobe dummy
modprobe: ERROR: could not insert 'dummy': Operation not permitted
```

Loading a module needs `CAP_SYS_MODULE`. The original plan had the daemon set
the module up on first run, which cannot work as a user service.

## Considered options

**Grant the daemon `CAP_SYS_MODULE`.** Rejected. That capability is
[effectively root](https://man7.org/linux/man-pages/man7/capabilities.7.html) —
it loads arbitrary kernel code — and a webcam helper is not something to hand it
to.

**A polkit action, or a small system service the user daemon asks.** Works, and
is what a tool needing genuinely dynamic module parameters would do. Rejected as
disproportionate: our parameters never change at runtime, so the privileged
component would exist to run one fixed command, and it is one more thing to
package, authorise and debug.

**Install-time configuration.** Chosen. The parameters are static, the AUR
package is already a privileged install step, and it removes the privilege
boundary from the running system entirely rather than mediating it.

## Consequences

The module is loaded from boot, so its node always exists. That is harmless
because of `exclusive_caps=1`: upstream says the device "will announce OUTPUT
capabilities only… as soon as you have attached a producer to the device, it
will start announcing CAPTURE capabilities only". An idle omavcam still does not
appear in anyone's camera dropdown.

Changing a module parameter now needs a reload, which needs root. Nothing in the
product does that today, and anything that wants to must be designed knowing it
is not a runtime operation.

**The node is found by its `card_label`, never by a hardcoded path.** `video_nr`
is a request, not a guarantee — another device can already hold that number, and
the load then fails or lands elsewhere. `/dev/video42` is a detail of one
machine's boot order, which is why `CONTEXT.md` lists it as a word to avoid.

Installation is no longer something the daemon can repair. A missing module is a
packaging failure to report clearly, not a condition to fix at runtime.
