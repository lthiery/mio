//! Both `poll(2)` and `event_port(2)` need to hold per-fd states.

use std::sync::atomic::{AtomicBool, Ordering};

cfg_io_source! {
use std::io;
#[cfg(not(target_os = "hermit"))]
use std::os::fd::RawFd;
// TODO: once <https://github.com/rust-lang/rust/issues/126198> is fixed this
// can use `std::os::fd` and be merged with the above.
#[cfg(target_os = "hermit")]
use std::os::hermit::io::RawFd;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use crate::sys::Selector;
use crate::{Interest, Registry, Token};
}

/// Shared record between IoSourceState and SelectorState that allows us to
/// internally deregister partially or fully closed fds (i.e. when we get
/// POLLHUP or PULLERR) without confusing IoSourceState and trying to deregister
/// twice.  This isn't strictly required as technically deregister is idempotent
/// but it is confusing when trying to debug behaviour as we get imbalanced
/// calls to register/deregister and superfluous NotFound errors.
#[derive(Debug)]
pub(crate) struct RegistrationRecord {
    is_unregistered: AtomicBool,
}

impl RegistrationRecord {
    pub(crate) fn new() -> RegistrationRecord {
        RegistrationRecord {
            is_unregistered: AtomicBool::new(false),
        }
    }

    pub(crate) fn mark_unregistered(&self) {
        self.is_unregistered.store(true, Ordering::Relaxed);
    }

    #[allow(dead_code)]
    pub(crate) fn is_registered(&self) -> bool {
        !self.is_unregistered.load(Ordering::Relaxed)
    }
}

cfg_io_source! {
pub(crate) struct IoSourceState {
    // Set once the source is registered, and cleared when it is deregistered.
    inner: OnceLock<Box<InternalState>>,
    // Serializes registration through a shared reference, and I/O that starts
    // before `inner` is set: the selector can deliver an event before
    // registration sets `inner`, and I/O that then returns `WouldBlock` must
    // be able to rearm.
    registering: Mutex<()>,
}

struct InternalState {
    selector: Selector,
    token: Token,
    interests: Interest,
    fd: RawFd,
    shared_record: Arc<RegistrationRecord>,
}
}

cfg_io_source! {
impl IoSourceState {
    pub(crate) fn new() -> IoSourceState {
        IoSourceState {
            inner: OnceLock::new(),
            registering: Mutex::new(()),
        }
    }

    pub(crate) fn do_io<T, F, R>(&self, f: F, io: &T) -> io::Result<R>
    where
        F: FnOnce(&T) -> io::Result<R>,
    {
        if let Some(state) = self.inner.get() {
            return Self::rearm_on_would_block(Some(&**state), f(io));
        }
        let _guard = self.registering.lock().unwrap_or_else(PoisonError::into_inner);
        Self::rearm_on_would_block(self.inner.get().map(Box::as_ref), f(io))
    }

    fn rearm_on_would_block<R>(
        state: Option<&InternalState>,
        result: io::Result<R>,
    ) -> io::Result<R> {
        if let Err(err) = &result {
            if err.kind() == io::ErrorKind::WouldBlock {
                state.map_or(Ok(()), |state| {
                    state
                        .selector
                        .reregister(state.fd, state.token, state.interests)
                })?;
            }
        }

        result
    }

    pub(crate) fn register(
        &self,
        registry: &Registry,
        token: Token,
        interests: Interest,
        fd: RawFd,
    ) -> io::Result<()> {
        let _guard = self.registering.lock().unwrap_or_else(PoisonError::into_inner);
        if self.inner.get().is_some() {
            return Err(io::ErrorKind::AlreadyExists.into());
        }

        let selector = registry.selector().try_clone()?;
        let shared_record = selector.register_internal(fd, token, interests)?;
        let state = InternalState {
            selector,
            token,
            interests,
            fd,
            shared_record,
        };
        // `inner` is empty and the lock is held, so this cannot fail.
        let _ = self.inner.set(Box::new(state));
        Ok(())
    }

    pub(crate) fn reregister(
        &mut self,
        registry: &Registry,
        token: Token,
        interests: Interest,
        fd: RawFd,
    ) -> io::Result<()> {
        match self.inner.get_mut() {
            Some(state) => registry
                .selector()
                .reregister(fd, token, interests)
                .map(|()| {
                    state.token = token;
                    state.interests = interests;
                }),
            None => Err(io::ErrorKind::NotFound.into()),
        }
    }

    pub(crate) fn deregister(&mut self, registry: &Registry, fd: RawFd) -> io::Result<()> {
        if let Some(state) = self.inner.take() {
            // Marking unregistered will short circuit the drop behaviour of calling
            // deregister so the call to deregister below is strictly required.
            state.shared_record.mark_unregistered();
        }

        registry.selector().deregister(fd)
    }
}

impl Drop for InternalState {
    fn drop(&mut self) {
        if self.shared_record.is_registered() {
            let _ = self.selector.deregister(self.fd);
        }
    }
}
}
