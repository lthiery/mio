# SUPERSEDED 2026-11-09 by ISSUE-shared-source.md (path A: #2029 is about arbitrary fds, not our case). Kept for the SourceFd rearm data point only.

Purpose: propose `Registry::register_shared` as the fix for the half of #2029
that is mio-owned types, say plainly that it does NOT fix `SourceFd`/`AsyncFd`,
and ask before opening the PR (Thomas closes undiscussed PRs, see #2020).
Branch to link if asked: lthiery/mio `shared-registration-v3` (TcpStream only
so far; the PR would cover every `IoSource`-backed type + `SourceFd`).

Keep out: Tokio sharding, io_uring, worker-local, the #1064 Carl quote (save
for the PR body). Thread participants: Thomasdezeeuw (author), Darksonn.

---

I think there are two separate reasons people end up registering through
`SourceFd` and losing the rearm, and one of them is fixable without touching
the selector or making `IoSource` public.

The first reason is that the fd is not a mio type at all (`AsyncFd`, the
Haiku case in #2018). Nothing short of an explicit rearm entry point fixes
that; that is what the `IoSource`-public / guard discussion above is about and
I have nothing to add to it.

The second reason is that the caller has a mio type but only a shared
reference to it when it registers: the socket is in an `Arc`, or split into
halves that may race to be the first to use it, or registration is deferred
until first use. `Source::register` takes `&mut self`, so the only way to
register from `&TcpStream` today is `SourceFd(&stream.as_raw_fd())`, and that
puts a perfectly good `IoSource` into the broken bucket for no reason.

Proposal: a second trait for initial registration through a shared reference,

```rust
pub trait SharedSource: Source {
    fn register_shared(&self, registry: &Registry, token: Token, interests: Interest) -> io::Result<()>;
}
impl Registry { pub fn register_shared<S: SharedSource + ?Sized>(&self, source: &S, token: Token, interests: Interest) -> io::Result<()>; }
```

implemented for every `IoSource`-backed type and for `SourceFd` (stateless, so
trivially). Same contract as `register`, including that registering an already
registered source is unspecified; the two guarantees added are that I/O may
run concurrently with the registration and that a failed registration can be
retried. Reregister and deregister stay `&mut`.

On epoll and kqueue `IoSourceState` is empty, so this costs nothing there:
`TcpStream` keeps its size and `do_io` is unchanged. On poll(2), event ports
and Windows the per-fd state is published through a `OnceLock` behind a
`Mutex<()>` that only I/O racing the registration itself ever takes; after
that the `do_io` path is the same load it is today. That grows those sockets
by a `OnceLock` + `Mutex<()>` (16 bytes on Linux poll), which I would rather
discuss than hide.

I have this working for `TcpStream` with tests on epoll and forced poll(2),
including a three-burst read that fails today when the stream is registered
via `SourceFd` and passes through `register_shared`, and the rest of the
suite is green on Linux and Windows. If the shape is acceptable I will finish
the other types and open a PR; if you would rather solve the shared-reference
case differently I would like to hear it before writing more.
