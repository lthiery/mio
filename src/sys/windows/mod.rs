mod afd;

pub mod event;
pub use event::{Event, Events};

mod handle;
use handle::Handle;

mod io_status_block;
mod iocp;

mod overlapped;
use overlapped::Overlapped;

mod selector;
pub use selector::Selector;

// Macros must be defined before the modules that use them
cfg_net! {
    /// Helper macro to execute a system call that returns an `io::Result`.
    //
    // Macro must be defined before any modules that use them.
    macro_rules! syscall {
        ($fn: ident ( $($arg: expr),* $(,)* ), $err_test: path, $err_value: expr) => {{
            let res = unsafe { $fn($($arg, )*) };
            if $err_test(&res, &$err_value) {
                Err(io::Error::last_os_error())
            } else {
                Ok(res)
            }
        }};
    }

    mod net;

    pub(crate) mod tcp;
    pub(crate) mod udp;

    pub use selector::{SelectorInner, SockState};
}

cfg_os_ext! {
    pub(crate) mod named_pipe;
}

mod waker;
pub(crate) use waker::Waker;

cfg_io_source! {
    use std::io;
    use std::os::windows::io::RawSocket;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex, OnceLock, PoisonError};

    use crate::{Interest, Registry, Token};

    struct InternalState {
        selector: Arc<SelectorInner>,
        token: Token,
        interests: Interest,
        sock_state: Pin<Arc<Mutex<SockState>>>,
    }

    impl Drop for InternalState {
        fn drop(&mut self) {
            let mut sock_state = self.sock_state.lock().unwrap();
            sock_state.mark_delete();
        }
    }

    pub struct IoSourceState {
        // Set once the socket is registered, and cleared when it is
        // deregistered.
        //
        // We box the internal state to not increase the size on the stack as the
        // type might move around a lot.
        inner: OnceLock<Box<InternalState>>,
        // Serializes registration through a shared reference, and I/O that
        // starts before `inner` is set: the selector can deliver an event
        // before registration sets `inner`, and I/O that then returns
        // `WouldBlock` must be able to rearm.
        registering: Mutex<()>,
    }

    impl IoSourceState {
        pub fn new() -> IoSourceState {
            IoSourceState {
                inner: OnceLock::new(),
                registering: Mutex::new(()),
            }
        }

        pub fn do_io<T, F, R>(&self, f: F, io: &T) -> io::Result<R>
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
            if let Err(ref e) = result {
                if e.kind() == io::ErrorKind::WouldBlock {
                    state.map_or(Ok(()), |state| {
                        state
                            .selector
                            .reregister(state.sock_state.clone(), state.token, state.interests)
                    })?;
                }
            }
            result
        }

        pub fn register(
            &self,
            registry: &Registry,
            token: Token,
            interests: Interest,
            socket: RawSocket,
        ) -> io::Result<()> {
            let _guard = self.registering.lock().unwrap_or_else(PoisonError::into_inner);
            if self.inner.get().is_some() {
                return Err(io::ErrorKind::AlreadyExists.into());
            }

            let state = registry.selector().register(socket, token, interests)?;
            // `inner` is empty and the lock is held, so this cannot fail.
            let _ = self.inner.set(Box::new(state));
            Ok(())
        }

        pub fn reregister(
            &mut self,
            registry: &Registry,
            token: Token,
            interests: Interest,
        ) -> io::Result<()> {
            match self.inner.get_mut() {
                Some(state) => {
                    registry
                        .selector()
                        .reregister(state.sock_state.clone(), token, interests)
                        .map(|()| {
                            state.token = token;
                            state.interests = interests;
                        })
                }
                None => Err(io::ErrorKind::NotFound.into()),
            }
        }

        pub fn deregister(&mut self) -> io::Result<()> {
            match self.inner.take() {
                Some(state) => {
                    let mut sock_state = state.sock_state.lock().unwrap();
                    sock_state.mark_delete();
                    Ok(())
                }
                None => Err(io::ErrorKind::NotFound.into()),
            }
        }
    }
}
