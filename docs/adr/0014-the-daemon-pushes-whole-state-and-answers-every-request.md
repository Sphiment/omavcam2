# The daemon pushes whole state, and still answers every request

The socket protocol is line-delimited JSON. The daemon pushes the **entire**
state object to every connected client whenever it changes, and once on connect.
Nothing polls. Alongside that, every request gets an explicit response carrying
the request's id, a success flag or machine-readable error, and a revision.

## Whole state, not deltas

A delta stream is smaller and wrong more often. It only makes sense to a client
that has the previous state, so a client that connects late, reconnects after
the daemon was restarted, or drops a message needs a resync request and the
daemon needs to answer it. Pushing everything deletes that whole path: a client
is correct the moment its first message arrives, and reconnecting is the only
recovery mechanism there is.

The state is a handful of fields. There is no volume argument on the other side.

## Push alone is not enough

This is the part that is expensive to retrofit, so it is here from the first
commit rather than from ticket 9.

A pushed state says what is true now. It does not say which request caused it,
or whether a request succeeded at all. With push only:

- `omavcam start` has nothing to derive an exit code from;
- Apply cannot report *which* setting the phone rejected;
- pairing cannot distinguish "failed" from "not yet".

So each request carries an `id` which its response echoes, and the response says
`ok` or names an error `code`. The CLI's exit status is that flag.

## Revisions tie a response to a state

A response also carries `rev`, the revision at which its effect is visible.
The daemon publishes before it responds, so by the time a client reads the
response it already holds the state that reflects it — and a client that reads
out of order can wait for `rev >= response.rev` rather than guessing.

Revisions are scoped to one daemon run and restart at 1. That is not a gap: a
reconnecting client is pushed the whole state before it can ask for anything, so
there is nothing for a revision to be compared against across the restart.

## The two limits are protocol, not implementation detail

Every message carries `v`. A client speaking another version is rejected with
`unsupported_version` rather than silently misreading a field that changed
meaning — the failure mode a version field exists to prevent.

Version 2 is the first shape with the connection machine and typed capture
state. Version 1 reserved a `phone: null` field and was changed incompatibly
when that became `connection`; bumping here makes the promised failure explicit
before separately installed QML clients exist. Clients validate the daemon's
version as well as the daemon validating theirs.

Requests are bounded at 64 KiB, enforced while reading rather than after, so a
malformed or hostile client cannot make the daemon allocate without limit.

## Consequences

Clients are dumb: connect, read state, render. Every surface — CLI, bar widget,
Studio — renders the same object, and no surface has resync logic in it.

Client sockets are read on separate threads, but every state-changing operation
passes through one transition turnstile. This is part of the request semantics:
two callers selecting different phones or racing start against stop must be
ordered, not both told that their incompatible effects succeeded.

The daemon writes to clients while holding the state lock so revisions cannot
arrive out of order. Each socket has a one-second write timeout: a client that
stops reading is dropped instead of freezing every transition. A per-client
queue remains the upgrade if state volume ever makes that timeout visible.
