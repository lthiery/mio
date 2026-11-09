# DRAFT: mio PR "Keep the debug registry association on failed registrations". NOT POSTED.

Branch `registry-association` on lthiery/mio, 1 commit 8021e7c (amended, no
cherry-pick trailer) on upstream master 5e2b24a. Pushed 2026-11-09.
Compare: https://github.com/tokio-rs/mio/compare/master...lthiery:mio:registry-association

Verified on this exact branch: full `cargo test --all-features` (debug), new
tests in release and under `--cfg mio_unsupported_force_poll_poll`, clippy
`-D warnings` clean, fmt, MSRV 1.71, windows-msvc cross-check. Both new tests
fail on master. Behaviour dates from a19478d (2020-01), so every mio 0.7+.

No CHANGELOG entry (maintainers add at release). No issue first: it is a
debug-assertion bug with failing tests, not a design question.

---

## Title

Keep the debug registry association on failed registrations

## Body

In debug builds `IoSource` records which `Registry` it is registered with, to
catch misuse. Two paths in `register` corrupt that record:

- Registering a source that is already registered with registry A, with a
  second registry B, is rejected as it should be, but the record now says B.
  Deregistering from A then fails with `NotFound`, and the fd stays in A's
  selector until it is closed.
- A registration the selector rejects (for example EEXIST, the fd is already
  registered through a `SourceFd`) keeps the record. Retrying it, which the
  error contract allows, fails with `AlreadyExists` in debug builds while
  the same program succeeds in release.

The fix makes `associate` a compare-exchange from "unassociated" instead of a
swap, and undoes the association when the selector call fails. Reregister
and deregister are unchanged. Release builds are unaffected (`SelectorId`
does not exist there), there is no API change.

Tests: `tcp_register_multiple_event_loops` now deregisters from the winning
registry at the end (fails on master), and `tcp_register_retry_after_failure`
does the fail-then-retry on Linux, where a duplicate epoll registration is
an error (fails on master). Both pass on epoll and on the forced poll(2)
selector, debug and release.

Found while writing a test that registers the same descriptor twice.
