#![cfg(all(feature = "os-poll", feature = "net", not(target_os = "wasi")))]

use mio::{net::TcpStream, Events, Interest, Poll, Token};
use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

fn pair() -> (TcpStream, std::net::TcpStream) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let client = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (server, _) = listener.accept().unwrap();
    server.set_nonblocking(true).unwrap();
    (TcpStream::from_std(server), client)
}

fn readable(poll: &mut Poll, token: Token) {
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut events = Events::with_capacity(16);
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .expect("readiness did not rearm");
        poll.poll(&mut events, Some(remaining)).unwrap();
        if events
            .iter()
            .any(|event| event.token() == token && event.is_readable())
        {
            return;
        }
    }
}

#[test]
fn shared_registration_rearms_and_supports_exclusive_lifecycle() {
    let (mut stream, mut peer) = pair();
    let mut poll = Poll::new().unwrap();
    poll.registry()
        .register_shared(&stream, Token(1), Interest::READABLE)
        .unwrap();
    for byte in *b"ab" {
        peer.write_all(&[byte]).unwrap();
        readable(&mut poll, Token(1));
        let mut buf = [0];
        (&stream).read_exact(&mut buf).unwrap();
        assert_eq!(buf, [byte]);
        assert_eq!(
            (&stream).read(&mut buf).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }
    poll.registry()
        .reregister(&mut stream, Token(2), Interest::READABLE)
        .unwrap();
    peer.write_all(b"c").unwrap();
    readable(&mut poll, Token(2));
    (&stream).read_exact(&mut [0]).unwrap();
    poll.registry().deregister(&mut stream).unwrap();
    let mut second = Poll::new().unwrap();
    second
        .registry()
        .register_shared(&stream, Token(3), Interest::READABLE)
        .unwrap();
    peer.write_all(b"d").unwrap();
    readable(&mut second, Token(3));
    second.registry().deregister(&mut stream).unwrap();
}

// Racing registrations are the caller's error to avoid; debug builds detect
// them without disturbing the registration that won.
#[cfg(debug_assertions)]
#[test]
fn racing_registration_is_detected_in_debug_builds() {
    use std::sync::{Arc, Barrier};

    let (stream, _peer) = pair();
    let stream = Arc::new(stream);
    let barrier = Arc::new(Barrier::new(3));
    let first = Poll::new().unwrap();
    let second = Poll::new().unwrap();
    let attempts: Vec<_> = [first.registry(), second.registry()]
        .into_iter()
        .map(|registry| {
            let registry = registry.try_clone().unwrap();
            let stream = stream.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                registry.register_shared(&*stream, Token(1), Interest::READABLE)
            })
        })
        .collect();
    barrier.wait();
    let results: Vec<_> = attempts
        .into_iter()
        .map(|attempt| attempt.join().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(
                |result| matches!(result, Err(err) if err.kind() == io::ErrorKind::AlreadyExists)
            )
            .count(),
        1
    );
    // The rejected registry must not replace the winning association.
    let mut stream = Arc::try_unwrap(stream).unwrap();
    let winner = if results[0].is_ok() { &first } else { &second };
    winner.registry().deregister(&mut stream).unwrap();
}

#[cfg(all(feature = "os-ext", any(target_os = "linux", target_os = "android")))]
#[test]
fn failed_os_registration_can_be_retried() {
    use std::os::fd::AsRawFd;
    let (mut stream, _peer) = pair();
    let poll = Poll::new().unwrap();
    let fd = stream.as_raw_fd();
    let mut source = mio::unix::SourceFd(&fd);
    poll.registry()
        .register(&mut source, Token(0), Interest::READABLE)
        .unwrap();
    assert_eq!(
        poll.registry()
            .register_shared(&stream, Token(1), Interest::READABLE)
            .unwrap_err()
            .kind(),
        io::ErrorKind::AlreadyExists
    );
    poll.registry().deregister(&mut source).unwrap();
    poll.registry()
        .register_shared(&stream, Token(1), Interest::READABLE)
        .unwrap();
    poll.registry().deregister(&mut stream).unwrap();
}
