# Shared initial registration prototype

Updated 2026-11-06 (v3). Local design notes, not an upstream PR description.

## What we are trying to do

Allow Tokio to defer registering a TCP stream with its I/O driver until the
task using that stream first attempts I/O or polls readiness. Mio should provide
the platform abstraction needed to register the actual socket through a shared
reference, including all backend state required for subsequent I/O.

The prototype lives on Mio branch `shared-registration-v3`, based on upstream
`5e2b24a`, as two commits:

- `19523e3` Keep the debug registry association on failed registrations. A
  standalone fix to existing `register` behavior, with tests that fail on
  upstream; it could go upstream on its own.
- `3eaf28e` Add shared registration for TCP streams.

The first prototype (`shared-registration`, `47aaad0`) put the lock and state in
the generic `IoSource` and is superseded. The motivating Tokio branch is
`tcp-deferred-registration` in `/home/louis/tokio-lazy-reg`. Tokio has not yet
been adapted to this Mio API.

## Why defer registration?

An accepted stream is normally registered by the task running the accept loop,
before the task that will handle the connection exists. Deferring registration
lets the connection task perform it on first use.

This has three motivations:

- For a future sharded I/O driver, first use provides a point at which to choose
  a shard using the connection task's execution context. It is a placement hook,
  not a guarantee of stable placement: work stealing can move the task, and
  experiments with the related Tokio sharding branch found that affinity was
  needed for stable placement.
- It moves registration work out of the accept loop and into connection tasks.
  Existing Tokio branch measurements suggest a benefit for a multiworker
  accept-and-spawn workload; this Mio prototype has not been benchmarked.
- Streams constructed and converted back to standard-library sockets without
  I/O can avoid registration entirely.

## Why Mio needs a change

Mio's existing `Source::register` requires `&mut self`. Tokio's first-use paths
have a shared reference to the stream, and split read/write halves can race to
use it. Obtaining a mutable reference to the socket at that point would violate
the ownership model unless the surrounding design changes.

The original Tokio implementation bypassed the socket wrapper on Unix by
registering a `SourceFd`. On non-Unix targets it retained eager registration.
That shortcut does not preserve the abstraction across Mio backends:

| Backend | Registration state needed by the socket wrapper |
| --- | --- |
| epoll / kqueue | No backend rearming state; debug builds track selector association |
| poll / Solaris event ports | Selector, token, interests, and registration record used to rearm notifications |
| Windows | Selector, token, interests, and socket polling state used to rearm notifications |

Registering only the descriptor leaves the wrapper's rearming state
uninitialized on stateful backends. After an event is consumed and I/O returns
`WouldBlock`, subsequent readiness notifications can stop arriving.

A two-burst read reproducer confirmed this regression using Mio's forced poll
backend: it passed on Tokio base `695a07dd`, timed out on the deferred branch,
and passed on the deferred branch with epoll. The solution should initialize the
actual Mio socket's state rather than extend the raw-descriptor workaround.

## Prototype API and implementation

The API is additive:

```rust
registry.register_shared(&stream, token, interests)?;
```

`event::SharedSource: Source` adds `register_shared(&self, ...)`, and
`Registry::register_shared` delegates to it. `IoSource` implements it;
`mio::net::TcpStream` is the only socket type that exposes it so far.

The contract is `register`'s, plus two guarantees:

- I/O on the source may run concurrently with its registration.
- A failed registration may be retried.

Registering an already registered source stays unspecified behavior, as with
`register`, so callers that can race to register must serialize. Tokio has to
do that anyway, to set up its own per-resource driver state exactly once. Debug
builds detect a race without disturbing the winner. Re-registration and
deregistration still require exclusive access. Implementing `Source` for
`&TcpStream` would also expose shared re-registration and deregistration,
which is broader than needed.

The work is per backend, in `IoSourceState`, so a backend pays only for what it
needs:

- **epoll / kqueue / WASI** keep no per-source state. Backend `register` just
  takes `&self`; `IoSource` keeps upstream's layout and I/O path.
- **poll / Solaris event ports / Windows** publish their rearming state through
  a `OnceLock<Box<InternalState>>`, with a `Mutex<()>` that serializes
  registration. I/O on a registered source reads the `OnceLock` without
  locking. I/O that starts before publication takes the lock and holds it
  across the operation. That closes the window where the selector delivers an
  event before the state is published and a `WouldBlock` would fail to rearm.
  Deregistration takes the state, so the source can be registered again.

`IoSource::register_with` does the debug selector association. It only
associates a source that is not associated yet, and undoes the association
when the backend rejects the registration. Exclusive `Source::register`
delegates to the shared path. No new unsafe code.

Measured `size_of::<TcpStream>()` on Linux (UDP is identical):

| | upstream | prototype v1 | v3 |
| --- | --- | --- | --- |
| epoll release | 4 | 16 | 4 |
| epoll debug | 16 | 24 | 16 |
| forced poll release | 16 | 32 | 32 |
| forced poll debug | 24 | 40 | 40 |

Windows has the same structure as poll (a `Box` grows into a `OnceLock<Box>`
plus a `Mutex<()>`) but has not been measured.

## Validation completed

- Full Linux `cargo test --all-features` suite, debug, on epoll and forced
  poll (`RUSTFLAGS='--cfg mio_unsupported_force_poll_poll'`); the new tests
  also in release on both.
- Lifecycle test on both backends: repeated readiness after `WouldBlock`,
  token changes through exclusive re-registration, and deregistration followed
  by registration with another poller.
- Racing registration against two pollers, debug only: exactly one succeeds
  and the winner can still deregister.
- Registration failure and retry after removing a conflicting raw-FD
  registration, through both `register` and `register_shared`. Linux/Android
  only, where the duplicate is an error; kqueue updates the registration.
- Rust 1.71 MSRV check and formatting. Clippy with warnings denied is clean on
  epoll. Forced poll and Windows each fail clippy with 2 lints that upstream
  `5e2b24a` fails identically (`poll.rs`, `named_pipe.rs`), so they are not
  ours.
- Compile checks for Windows MSVC, FreeBSD, WASI preview 1 (library) and the
  shell configuration (`net` without `os-poll`).

Windows, FreeBSD, WASI, and Solaris runtime behavior has not been validated.
No test forces I/O into the registration/publication window; the argument
for that path is the lock, not a test.

## Costs and unresolved design work

- Stateful backends grow by a `OnceLock` and a `Mutex<()>` per source (16 bytes
  on Linux forced poll). This could shrink, for example by publishing through
  an `AtomicPtr`, at the cost of unsafe code. Not worth doing before upstream
  feedback.
- On stateful backends, I/O on an unregistered source now takes an
  uncontended mutex, held across the operation. Registration takes it too,
  including exclusive `register`. Not benchmarked on the stateful backends;
  epoll and kqueue are unaffected by construction. Through Tokio's
  `net_accept` bench on epoll (2026-11-06), `register_shared` is
  indistinguishable from the old `SourceFd` registration.
- Trait name SETTLED (2026-11-09): `SharedSource` / `register_shared`, not
  Carl's 2019 `SyncSource` (mio #1064). `SyncSource` described all three
  `Source` operations through `&self`; this trait is initial registration only,
  on purpose (keeps #1064's `&mut` door open, avoids concurrent-deregister
  semantics). `Sync` is also the wrong word: the property is the receiver, not
  thread safety. Cite the comment as precedent, scope cut explained in one
  sentence. Method stays `register_shared`, not `register`, so call sites with
  both traits in scope are never ambiguous and the asymmetry is visible at
  `Registry::register_shared`.
- Still to settle: which other socket types implement it in the first proposal
  (all `IoSource`-backed ones is the honest answer), plus `impl SharedSource
  for SourceFd` (stateless, trivially sound, completes the trait).
- Test Windows and other stateful backends at runtime, including races, error
  recovery, and repeated register/deregister cycles.
- Tokio integration: DONE locally, branch `tcp-shared-registration` in
  `/home/louis/tokio-lazy-reg` (2026-11-06). `PollEvented` stores a
  `fn(&E) -> &(dyn SharedSource + Sync)`; `registration()` returns a
  `RegistrationRef` that registers through it on first use; deregistration
  goes through the real `&mut io`. No `SourceFd`, no unix-only cfgs; Windows
  defers too. A three-burst read test fails on the old `SourceFd` branch with
  forced poll and passes with this API. The workspace points at this checkout
  through a DO NOT MERGE `[patch.crates-io]` commit.
- Out of scope but related: Tokio's `AsyncFd` registers a `SourceFd` too, and
  upstream Tokio already fails and hangs in `io_async_fd` on forced poll. An
  `AsyncFd` wraps an arbitrary descriptor, so this API does not cover it.
- Revisit Tokio's observable behavior and documentation: registration errors
  move to first use, metrics count registration later, and an unused stream
  first accessed after runtime shutdown cannot register.

The Tokio integration works, so the API shape covers what Tokio needs. Still
to do before upstream discussion: runtime validation on Windows, and deciding
which implementation costs need to come down.
