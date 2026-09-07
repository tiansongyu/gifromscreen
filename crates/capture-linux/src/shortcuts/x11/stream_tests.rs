use super::*;

#[test]
fn long_lived_update_restores_deadline_and_cancellation_without_changing_idle_registration() {
    let (client, _server) = UnixStream::pair().unwrap();
    let cancellation = AtomicBool::new(false);
    let stream = BoundedStream {
        inner: DefaultStream::from_unix_stream(client).unwrap().0,
        cancellation: &cancellation,
        deadline: Mutex::new(Some(
            Instant::now().checked_sub(Duration::from_secs(1)).unwrap(),
        )),
        cleanup: AtomicBool::new(false),
    };
    assert_eq!(stream.check().unwrap_err().kind(), io::ErrorKind::TimedOut);
    stream.registration_complete();
    assert!(stream.deadline.lock().unwrap().is_none());
    stream.check().unwrap();
    let before = Instant::now();
    stream.begin_operation();
    let deadline = stream.deadline.lock().unwrap().unwrap();
    assert!(deadline >= before + SETUP_TIME);
    assert!(deadline <= Instant::now() + SETUP_TIME);
    cancellation.store(true, Ordering::Release);
    assert_eq!(
        stream.check().unwrap_err().kind(),
        io::ErrorKind::ConnectionAborted
    );
    stream.begin_cleanup();
    stream.check().unwrap(); // Cleanup still has its cancellation-exempt grace.
    stream.begin_operation();
    assert_eq!(
        stream.check().unwrap_err().kind(),
        io::ErrorKind::ConnectionAborted
    );
}
#[test]
fn network_names_are_never_resolved_synchronously() {
    assert!(addresses("example.invalid", 6000).is_err());
    assert_eq!(addresses("localhost", 6010).unwrap().len(), 2);
    assert_eq!(
        addresses("127.0.0.1", 6010).unwrap()[0],
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 6010)
    );
    assert!(addresses("[::1]", 6010).is_ok());
}

#[test]
fn numeric_tcp_transport_connects_to_a_private_loopback_listener() {
    let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let cancellation = AtomicBool::new(false);
    let client = connect_tcp(
        address,
        &cancellation,
        Instant::now() + Duration::from_secs(1),
    )
    .unwrap();
    assert_eq!(client.peer_addr().unwrap(), address);
    let (_server, _) = listener.accept().unwrap();
}
#[test]
fn auth_parser_requires_matching_peer_display_and_protocol() {
    let mut bytes = 256_u16.to_be_bytes().to_vec();
    for field in [
        &b"host"[..],
        &b"19"[..],
        &b"MIT-MAGIC-COOKIE-1"[..],
        &[7; 16][..],
    ] {
        bytes.extend_from_slice(&u16::try_from(field.len()).unwrap().to_be_bytes());
        bytes.extend_from_slice(field);
    }
    assert_eq!(
        parse_auth(&bytes, Family::LOCAL, b"host", 19).unwrap().1,
        [7; 16]
    );
    assert!(
        parse_auth(&bytes, Family::LOCAL, b"other", 19)
            .unwrap()
            .1
            .is_empty()
    );
    assert!(
        parse_auth(&bytes, Family::LOCAL, b"host", 20)
            .unwrap()
            .1
            .is_empty()
    );
    assert!(parse_auth(&bytes[..bytes.len() - 1], Family::LOCAL, b"host", 19).is_err());
}

#[test]
fn blocked_handshake_responds_to_cancellation_without_a_server_response() {
    use std::sync::{Arc, mpsc};
    let (client, _server) = UnixStream::pair().unwrap();
    let cancellation = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&cancellation);
    let (sender, receiver) = mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || {
        let stream = BoundedStream {
            inner: DefaultStream::from_unix_stream(client).unwrap().0,
            cancellation: &flag,
            deadline: Mutex::new(Some(Instant::now() + SETUP_TIME)),
            cleanup: AtomicBool::new(false),
        };
        let result = super::super::Client::connect_to_stream(stream, 0)
            .map(|_| ())
            .map_err(|e| e.to_string());
        sender.send(result).unwrap();
    });
    std::thread::sleep(Duration::from_millis(30));
    let started = Instant::now();
    cancellation.store(true, Ordering::Release);
    assert!(
        receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap_err()
            .contains("cancelled")
    );
    assert!(started.elapsed() < Duration::from_millis(500));
    worker.join().unwrap();
}

#[test]
fn blocked_handshake_and_cleanup_have_finite_protocol_deadlines() {
    for cleanup in [false, true] {
        let (client, _server) = UnixStream::pair().unwrap();
        let cancellation = AtomicBool::new(cleanup);
        let stream = BoundedStream {
            inner: DefaultStream::from_unix_stream(client).unwrap().0,
            cancellation: &cancellation,
            deadline: Mutex::new(Some(Instant::now() + Duration::from_millis(40))),
            cleanup: AtomicBool::new(false),
        };
        if cleanup {
            stream.begin_cleanup();
        }
        let started = Instant::now();
        let result = super::super::Client::connect_to_stream(stream, 0)
            .map(|_| ())
            .map_err(|e| e.to_string());
        assert!(result.unwrap_err().contains("deadline"));
        assert!(started.elapsed() < Duration::from_millis(500));
    }
}
