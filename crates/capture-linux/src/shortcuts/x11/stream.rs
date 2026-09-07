//! Cancellable X11 setup without DNS or an unbounded protocol poll.

use rustix::{
    event::{PollFd, PollFlags, Timespec, poll},
    net::{AddressFamily, SocketAddrUnix, SocketFlags, SocketType},
};
use std::{
    fs::{self, OpenOptions},
    io::{self, IoSlice, Read},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream},
    os::unix::{fs::OpenOptionsExt, net::UnixStream},
    path::PathBuf,
    sync::{
        Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use x11rb::{
    rust_connection::{DefaultStream, PollMode, Stream},
    utils::RawFdContainer,
};
use x11rb_protocol::{
    parse_display::{ConnectAddress, parse_display},
    xauth::Family,
};

const SETUP_TIME: Duration = Duration::from_secs(5);
const POLL_TIME: Timespec = Timespec {
    tv_sec: 0,
    tv_nsec: 20_000_000,
};

pub(crate) struct BoundedStream<'a> {
    inner: DefaultStream,
    cancellation: &'a AtomicBool,
    deadline: Mutex<Option<Instant>>,
    cleanup: AtomicBool,
}
impl BoundedStream<'_> {
    pub(super) fn registration_complete(&self) {
        *self.deadline.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
    pub(crate) fn begin_cleanup(&self) {
        *self.deadline.lock().unwrap_or_else(PoisonError::into_inner) =
            Some(Instant::now() + Duration::from_millis(100));
        self.cleanup.store(true, Ordering::Release);
    }
    fn check(&self) -> io::Result<()> {
        if !self.cleanup.load(Ordering::Acquire) && self.cancellation.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "X11 shortcut operation cancelled",
            ));
        }
        if self
            .deadline
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "X11 shortcut protocol deadline exceeded",
            ));
        }
        Ok(())
    }
}
impl Stream for BoundedStream<'_> {
    fn poll(&self, mode: PollMode) -> io::Result<()> {
        loop {
            self.check()?;
            let mut flags = PollFlags::empty();
            if mode.readable() {
                flags |= PollFlags::IN;
            }
            if mode.writable() {
                flags |= PollFlags::OUT;
            }
            let mut fds = [PollFd::new(&self.inner, flags)];
            match poll(&mut fds, Some(&POLL_TIME)) {
                Ok(0) | Err(rustix::io::Errno::INTR) => {}
                Ok(_) => return self.check(),
                Err(error) => return Err(error.into()),
            }
        }
    }
    fn read(&self, bytes: &mut [u8], fds: &mut Vec<RawFdContainer>) -> io::Result<usize> {
        self.check()?;
        self.inner.read(bytes, fds)
    }
    fn write(&self, bytes: &[u8], fds: &mut Vec<RawFdContainer>) -> io::Result<usize> {
        self.check()?;
        self.inner.write(bytes, fds)
    }
    fn write_vectored(
        &self,
        bytes: &[IoSlice<'_>],
        fds: &mut Vec<RawFdContainer>,
    ) -> io::Result<usize> {
        self.check()?;
        self.inner.write_vectored(bytes, fds)
    }
}

pub(crate) fn connect<'a>(
    display: Option<&str>,
    cancellation: &'a AtomicBool,
) -> Result<super::Client<'a>, String> {
    let deadline = Instant::now() + SETUP_TIME;
    let parsed = parse_display(display).map_err(|e| format!("Invalid X11 DISPLAY: {e}"))?;
    if parsed.display > 59_535 {
        return Err("X11 display number exceeds the bounded transport range".into());
    }
    let mut failure = "No supported local X11 endpoint".to_owned();
    let targets = if parsed.protocol.as_deref() == Some("unix")
        && std::path::Path::new(&parsed.host).is_absolute()
    {
        // The protocol helper's generic connect_instruction uses display 0 for
        // direct-path displays. Never redirect an explicit private socket to X0.
        vec![ConnectAddress::Socket(parsed.host.clone())]
    } else {
        parsed.connect_instruction().collect()
    };
    for address in targets {
        check(cancellation, deadline)?;
        match transport(&address, cancellation, deadline) {
            Ok((inner, (family, peer))) => {
                let (name, data) =
                    authentication(family, &peer, parsed.display, cancellation, deadline)?;
                let stream = BoundedStream {
                    inner,
                    cancellation,
                    deadline: Mutex::new(Some(deadline)),
                    cleanup: AtomicBool::new(false),
                };
                return super::Client::connect_to_stream_with_auth_info(
                    stream,
                    usize::from(parsed.screen),
                    name,
                    data,
                )
                .map_err(|e| format!("X11 shortcut setup failed: {e}"));
            }
            Err(error) => failure = error,
        }
    }
    Err(failure)
}

fn check(cancellation: &AtomicBool, deadline: Instant) -> Result<(), String> {
    if cancellation.load(Ordering::Acquire) {
        Err("X11 shortcut setup cancelled".into())
    } else if Instant::now() >= deadline {
        Err("X11 shortcut setup exceeded five seconds".into())
    } else {
        Ok(())
    }
}

fn addresses(host: &str, port: u16) -> Result<Vec<SocketAddr>, String> {
    if host == "localhost" {
        return Ok(vec![
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port),
            SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), port),
        ]);
    }
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    let ip=host.parse::<IpAddr>().map_err(|_|"X11 shortcuts accept local, localhost or numeric TCP DISPLAY addresses; DNS hostnames are unsupported to keep setup cancellable")?;
    Ok(vec![SocketAddr::new(ip, port)])
}

fn transport(
    address: &ConnectAddress<'_>,
    cancellation: &AtomicBool,
    deadline: Instant,
) -> Result<(DefaultStream, (Family, Vec<u8>)), String> {
    match address {
        ConnectAddress::Socket(path) => {
            let socket = rustix::net::socket_with(
                AddressFamily::UNIX,
                SocketType::STREAM,
                SocketFlags::NONBLOCK | SocketFlags::CLOEXEC,
                None,
            )
            .map_err(|e| e.to_string())?;
            let target = SocketAddrUnix::new(path.as_str()).map_err(|e| e.to_string())?;
            match rustix::net::connect(&socket, &target) {
                Ok(()) => {}
                Err(rustix::io::Errno::INPROGRESS) => loop {
                    check(cancellation, deadline)?;
                    let mut fds = [PollFd::new(&socket, PollFlags::OUT)];
                    match poll(&mut fds, Some(&POLL_TIME)) {
                        Ok(0) | Err(rustix::io::Errno::INTR) => {}
                        Ok(_) => break,
                        Err(e) => return Err(e.to_string()),
                    }
                },
                Err(error) => {
                    return Err(format!(
                        "Could not connect local X11 shortcut socket: {error}"
                    ));
                }
            }
            let socket = UnixStream::from(socket);
            if let Some(error) = socket.take_error().map_err(|e| e.to_string())? {
                return Err(error.to_string());
            }
            socket.peer_addr().map_err(|e| e.to_string())?;
            DefaultStream::from_unix_stream(socket).map_err(|e| e.to_string())
        }
        ConnectAddress::Hostname(host, port) => {
            let mut failure = "No X11 TCP endpoint accepted the connection".to_owned();
            for address in addresses(host, *port)? {
                check(cancellation, deadline)?;
                match connect_tcp(address, cancellation, deadline) {
                    Ok(socket) => {
                        return DefaultStream::from_tcp_stream(socket).map_err(|e| e.to_string());
                    }
                    Err(error) => failure = error,
                }
            }
            Err(failure)
        }
        _ => Err("Unsupported X11 shortcut transport".into()),
    }
}

fn connect_tcp(
    address: SocketAddr,
    cancellation: &AtomicBool,
    deadline: Instant,
) -> Result<TcpStream, String> {
    let family = if address.is_ipv4() {
        AddressFamily::INET
    } else {
        AddressFamily::INET6
    };
    let socket = rustix::net::socket_with(
        family,
        SocketType::STREAM,
        SocketFlags::NONBLOCK | SocketFlags::CLOEXEC,
        None,
    )
    .map_err(|e| e.to_string())?;
    match rustix::net::connect(&socket, &address) {
        Ok(()) => {}
        Err(rustix::io::Errno::INPROGRESS) => loop {
            check(cancellation, deadline)?;
            let mut fds = [PollFd::new(&socket, PollFlags::OUT)];
            match poll(&mut fds, Some(&POLL_TIME)) {
                Ok(0) | Err(rustix::io::Errno::INTR) => {}
                Ok(_) => break,
                Err(error) => return Err(error.to_string()),
            }
        },
        Err(error) => return Err(error.to_string()),
    }
    let stream = TcpStream::from(socket);
    if let Some(error) = stream.take_error().map_err(|e| e.to_string())? {
        return Err(error.to_string());
    }
    stream.peer_addr().map_err(|e| e.to_string())?;
    Ok(stream)
}

fn authentication(
    family: Family,
    address: &[u8],
    display: u16,
    cancellation: &AtomicBool,
    deadline: Instant,
) -> Result<(Vec<u8>, Vec<u8>), String> {
    let path = std::env::var_os("XAUTHORITY")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|dir| PathBuf::from(dir).join(".Xauthority")));
    let Some(path) = path else {
        return Ok((Vec::new(), Vec::new()));
    };
    let metadata = match fs::metadata(&path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok((Vec::new(), Vec::new())),
        Err(_) => return Err("Could not inspect X11 authentication file".into()),
    };
    if !metadata.is_file() || metadata.len() > 1024 * 1024 {
        return Err("X11 authentication must be a regular file no larger than 1 MiB".into());
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(
            i32::try_from(rustix::fs::OFlags::NONBLOCK.bits()).expect("NONBLOCK fits i32"),
        )
        .open(path)
        .map_err(|_| "Could not read X11 authentication file")?;
    let opened = file
        .metadata()
        .map_err(|_| "Could not inspect opened X11 authentication file")?;
    if !opened.is_file() || opened.len() > 1024 * 1024 {
        return Err("X11 authentication changed to an unsupported file".into());
    }
    let mut bytes = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        check(cancellation, deadline)?;
        let count = file
            .read(&mut buffer)
            .map_err(|_| "Could not read X11 authentication file")?;
        if count == 0 {
            break;
        }
        if bytes.len() + count > 1024 * 1024 {
            return Err("X11 authentication file grew beyond 1 MiB".into());
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    parse_auth(&bytes, family, address, display)
}

#[cfg(test)]
#[path = "stream_tests.rs"]
mod tests;

fn parse_auth(
    mut bytes: &[u8],
    family: Family,
    address: &[u8],
    display: u16,
) -> Result<(Vec<u8>, Vec<u8>), String> {
    fn field<'a>(bytes: &mut &'a [u8]) -> Result<&'a [u8], String> {
        let length = word(bytes)?;
        if bytes.len() < usize::from(length) {
            return Err("Malformed X11 authentication entry".into());
        }
        let (value, rest) = bytes.split_at(usize::from(length));
        *bytes = rest;
        Ok(value)
    }
    fn word(bytes: &mut &[u8]) -> Result<u16, String> {
        if bytes.len() < 2 {
            return Err("Truncated X11 authentication entry".into());
        }
        let value = u16::from_be_bytes([bytes[0], bytes[1]]);
        *bytes = &bytes[2..];
        Ok(value)
    }
    let number = display.to_string();
    while !bytes.is_empty() {
        let entry_family = Family::from(word(&mut bytes)?);
        let peer = field(&mut bytes)?;
        let entry_number = field(&mut bytes)?;
        let name = field(&mut bytes)?;
        let data = field(&mut bytes)?;
        if (entry_family == Family::WILD || (entry_family == family && peer == address))
            && (entry_number.is_empty() || entry_number == number.as_bytes())
            && name == b"MIT-MAGIC-COOKIE-1"
        {
            return Ok((name.to_vec(), data.to_vec()));
        }
    }
    Ok((Vec::new(), Vec::new()))
}
