# DRAFT: mio issue proposing `SharedSource`. NOT POSTED. Replaces COMMENT-2029.md (path A, 2026-11-09).

Why an issue and not a PR: Thomas closes undiscussed PRs (#2020). This asks
for the shape first. Branch to link only if asked:
lthiery/mio `shared-registration-v3` (TcpStream only; PR would cover all
IoSource types + SourceFd).

Keep out: Tokio sharding, io_uring, worker-local, #2029 (not their problem),
the Tokio lazy branch. One sentence of consumer motivation is enough.

---

## Title

Initial registration through a shared reference

## Body

`event::Source::register` takes `&mut self`. A consumer that registers a
socket on its first use, from a poll, only has `&self` at that point, so today
it has to register `SourceFd(&socket.as_raw_fd())` instead of the socket. That
works on epoll and kqueue and silently breaks on poll(2) and Windows, where
the socket's `IoSource` state is what rearms interest after `WouldBlock` and a
`SourceFd` registration never sets it up.

This is the case #1064 anticipated when `register` went to `&mut`: the
concern there was a `Sync` consumer registering from `&self`, and the answer
was "probably a new trait" (`SyncSource` was the working name). It was shelved
because Tokio registers eagerly, which is still true, but a consumer that
wants first-use registration has nothing to call.

Proposal: a trait for initial registration only,

```rust
pub trait SharedSource: Source {
    fn register_shared(&self, registry: &Registry, token: Token, interests: Interest) -> io::Result<()>;
}

impl Registry {
    pub fn register_shared<S: SharedSource + ?Sized>(&self, source: &S, token: Token, interests: Interest) -> io::Result<()>;
}
```

implemented for every `IoSource`-backed type and for `SourceFd`. Same
contract as `register`: registering an already registered source stays
unspecified, so callers that can race serialize themselves. Two things are
added: I/O may run concurrently with the registration, and a failed
registration may be retried. `reregister` and `deregister` stay `&mut`, so
the door #1064 opened for lock-free implementations stays open.

Cost: nothing on epoll and kqueue, where `IoSourceState` is empty; `TcpStream`
keeps its size and `do_io` is unchanged. On poll(2), event ports and Windows
the per-fd state is published through a `OnceLock`, with a `Mutex<()>` that
only I/O racing the registration itself takes. That is 16 bytes per socket on
Linux poll(2), which I would rather state than hide.

I have this working for `TcpStream` with tests on epoll and forced poll(2),
suite green on Linux and Windows. Happy to open the PR if the shape is
acceptable, or to hear how you would rather cover the shared-reference case.
