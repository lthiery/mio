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

I'd like Tokio to register a `TcpStream` with the driver on its first use instead of at construction. The task that accepts a connection is usually not the one that drives it, so today the accept loop pays the `epoll_ctl` for every connection. In a Tokio branch, moving it to first use is worth about 13% on the accept benchmark at 4 workers. It would also decide which shard a socket lands on if the driver is sharded, as proposed in tokio-rs/tokio#8450, but the request does not depend on that.

First use happens in a poll, where Tokio only has `&TcpStream`. `Source::register` takes `&mut self`, so the only way to register is `SourceFd`, and that skips the `IoSource` state that rearms interest on poll(2) and Windows. #1064 expected this to need a new trait if Tokio ever stopped registering eagerly.

Proposal, initial registration only:

```rust
pub trait SharedSource: Source {
    fn register_shared(&self, registry: &Registry, token: Token, interests: Interest) -> io::Result<()>;
}
```

plus `Registry::register_shared`. Same contract as `register`; `reregister` and `deregister` stay `&mut`. It costs nothing on epoll and kqueue. On poll(2), event ports and Windows the per-fd state grows by 16 bytes (a `OnceLock` and a `Mutex<()>` taken only by I/O racing the registration).

I have it working for `TcpStream` on Linux and Windows, with the Tokio side on top. Is this a shape you'd take a PR for?
