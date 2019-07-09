extern crate env_logger;
extern crate futures;
extern crate libc;
extern crate mio;
extern crate net2;
extern crate tokio_io;
extern crate tokio_tcp;

use std::sync::mpsc::channel;
use std::{net, thread};

use futures::{Future, Stream};
use tokio_tcp::{TcpListener, TcpStream};

macro_rules! t {
    ($e:expr) => {
        match $e {
            Ok(e) => e,
            Err(e) => panic!("{} failed with {:?}", stringify!($e), e),
        }
    };
}

#[test]
fn connect() {
    drop(env_logger::try_init());
    let srv = t!(net::TcpListener::bind("127.0.0.1:0"));
    let addr = t!(srv.local_addr());
    let t = thread::spawn(move || t!(srv.accept()).0);

    let stream = TcpStream::connect(&addr);
    let mine = t!(stream.wait());
    let theirs = t.join().unwrap();

    assert_eq!(t!(mine.local_addr()), t!(theirs.peer_addr()));
    assert_eq!(t!(theirs.local_addr()), t!(mine.peer_addr()));
}

#[test]
fn accept() {
    drop(env_logger::try_init());
    let srv = t!(TcpListener::bind(&t!("127.0.0.1:0".parse())));
    let addr = t!(srv.local_addr());

    let (tx, rx) = channel();
    let client = srv
        .incoming()
        .map(move |t| {
            tx.send(()).unwrap();
            t
        })
        .into_future()
        .map_err(|e| e.0);
    assert!(rx.try_recv().is_err());
    let t = thread::spawn(move || net::TcpStream::connect(&addr).unwrap());

    let (mine, _remaining) = t!(client.wait());
    let mine = mine.unwrap();
    let theirs = t.join().unwrap();

    assert_eq!(t!(mine.local_addr()), t!(theirs.peer_addr()));
    assert_eq!(t!(theirs.local_addr()), t!(mine.peer_addr()));
}

#[test]
fn accept2() {
    drop(env_logger::try_init());
    let srv = t!(TcpListener::bind(&t!("127.0.0.1:0".parse())));
    let addr = t!(srv.local_addr());

    let t = thread::spawn(move || net::TcpStream::connect(&addr).unwrap());

    let (tx, rx) = channel();
    let client = srv
        .incoming()
        .map(move |t| {
            tx.send(()).unwrap();
            t
        })
        .into_future()
        .map_err(|e| e.0);
    assert!(rx.try_recv().is_err());

    let (mine, _remaining) = t!(client.wait());
    mine.unwrap();
    t.join().unwrap();
}

#[cfg(target_os = "linux")]
mod linux {
    use tokio_tcp::TcpStream;

    use env_logger;
    use futures::{future, Future};
    use mio::unix::UnixReady;
    use net2::TcpStreamExt;
    use tokio_io::AsyncRead;

    use std::io::Write;
    use std::time::Duration;
    use std::{net, thread};

    #[test]
    fn poll_hup() {
        drop(env_logger::try_init());

        let srv = t!(net::TcpListener::bind("127.0.0.1:0"));
        let addr = t!(srv.local_addr());
        let t = thread::spawn(move || {
            let mut client = t!(srv.accept()).0;
            client.set_linger(Some(Duration::from_millis(0))).unwrap();
            client.write(b"hello world").unwrap();
            thread::sleep(Duration::from_millis(200));
        });

        let mut stream = t!(TcpStream::connect(&addr).wait());

        // Poll for HUP before reading.
        future::poll_fn(|| stream.poll_read_ready(UnixReady::hup().into()))
            .wait()
            .unwrap();

        // Same for write half
        future::poll_fn(|| stream.poll_write_ready())
            .wait()
            .unwrap();

        let mut buf = vec![0; 11];

        // Read the data
        future::poll_fn(|| stream.poll_read(&mut buf))
            .wait()
            .unwrap();

        assert_eq!(b"hello world", &buf[..]);

        t.join().unwrap();
    }
}

#[cfg(unix)]
mod unix {
    use futures::future;
    use futures::{Async, Future};
    use libc::{connect, dup, sockaddr, sockaddr_in, socket, AF_INET, EMFILE, SOCK_STREAM};
    use std::io::Error;
    use std::mem;
    use std::net::SocketAddr;
    use tokio_tcp::TcpListener;

    #[test]
    fn listen_failure() {
        future::lazy(|| {
            // Set up a tokio TCP listener
            let addr = t!("127.0.0.1:0".parse::<SocketAddr>());
            let mut srv = t!(TcpListener::bind(&addr));
            let port = t!(srv.local_addr()).port();

            // Poll once to set up the reactor before we use all the FDs
            match srv.poll_accept() {
                Ok(Async::NotReady) => (),
                _ => panic!("unexpected poll result"),
            };

            // Create socket and consume all FDs allowed by this process
            let sock = unsafe { socket(AF_INET, SOCK_STREAM, 0) };
            assert!(sock != -1);
            loop {
                if unsafe { dup(sock) } == -1 {
                    let err = Error::last_os_error();
                    assert_eq!(err.raw_os_error(), Some(EMFILE));
                    break;
                }
            }

            // The listener should still be okay so far
            match srv.poll_accept() {
                Ok(Async::NotReady) => (),
                _ => panic!("unexpected poll result"),
            };

            // Use the original socket to connect
            let sa_len = mem::size_of::<sockaddr_in>() as u32;
            let sa = sockaddr_in {
                sin_family: AF_INET as u8,
                sin_port: port.to_be(),
                sin_addr: libc::in_addr {
                    s_addr: 0x7f000001_u32.to_be(), // 127.0.0.1
                },
                sin_zero: [0; 8],
                sin_len: sa_len as u8,
            };

            // This will succeed, but we can't accept it
            let addr_ptr: *const sockaddr = unsafe { mem::transmute(&sa) };
            let connect_result = unsafe { connect(sock, addr_ptr, sa_len) };
            assert_eq!(connect_result, 0);

            // There is no FD available to represent an incoming connection
            let res = future::poll_fn(|| srv.poll_accept()).wait();
            assert!(res.is_err());

            future::ok::<(), ()>(())
        })
        .wait()
        .unwrap();
    }
}
