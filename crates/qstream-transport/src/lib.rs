//! Socket transports for the qprofiler layer.

use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("no address resolved for {0}")]
    NoAddress(String),
}

pub trait Stream: Read + Write {
    fn set_read_timeout(&self, dur: Option<Duration>) -> io::Result<()>;
}

impl Stream for TcpStream {
    fn set_read_timeout(&self, dur: Option<Duration>) -> io::Result<()> {
        TcpStream::set_read_timeout(self, dur)
    }
}

#[cfg(unix)]
impl Stream for std::os::unix::net::UnixStream {
    fn set_read_timeout(&self, dur: Option<Duration>) -> io::Result<()> {
        std::os::unix::net::UnixStream::set_read_timeout(self, dur)
    }
}

#[derive(Clone, Debug)]
pub enum Target {
    Tcp(String),
    /// "@name" is an abstract socket, anything else is a filesystem path.
    Unix(String),
}

impl Target {
    pub fn tcp(addr: &str) -> Self {
        Target::Tcp(addr.to_string())
    }
}

pub fn connect(
    target: &Target,
    connect_timeout: Duration,
    read_timeout: Duration,
) -> Result<Box<dyn Stream>, Error> {
    match target {
        Target::Tcp(addr) => connect_tcp(addr, connect_timeout, read_timeout),
        Target::Unix(name) => connect_unix(name, read_timeout),
    }
}

pub fn connect_wait(
    target: &Target,
    connect_timeout: Duration,
    read_timeout: Duration,
    wait: Duration,
) -> Result<Box<dyn Stream>, Error> {
    let deadline = Instant::now() + wait;
    loop {
        match connect(target, connect_timeout, read_timeout) {
            Ok(stream) => return Ok(stream),
            Err(err) => {
                if Instant::now() >= deadline {
                    return Err(err);
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

fn connect_tcp(
    addr: &str,
    connect_timeout: Duration,
    read_timeout: Duration,
) -> Result<Box<dyn Stream>, Error> {
    let mut last = None;
    for sock in addr
        .to_socket_addrs()
        .map_err(|_| Error::NoAddress(addr.to_string()))?
    {
        match TcpStream::connect_timeout(&sock, connect_timeout) {
            Ok(stream) => {
                stream.set_read_timeout(Some(read_timeout))?;
                stream.set_nodelay(true)?;
                return Ok(Box::new(stream));
            }
            Err(err) => last = Some(err),
        }
    }
    Err(Error::Io(last.unwrap_or_else(|| {
        io::Error::new(io::ErrorKind::AddrNotAvailable, "empty address list")
    })))
}

#[cfg(unix)]
fn connect_unix(name: &str, read_timeout: Duration) -> Result<Box<dyn Stream>, Error> {
    use std::os::unix::net::UnixStream;

    let stream = if let Some(abstract_name) = name.strip_prefix('@') {
        #[cfg(target_os = "linux")]
        {
            use std::os::linux::net::SocketAddrExt;
            let addr =
                std::os::unix::net::SocketAddr::from_abstract_name(abstract_name.as_bytes())?;
            UnixStream::connect_addr(&addr)?
        }
        #[cfg(not(target_os = "linux"))]
        {
            return Err(Error::Io(io::Error::new(
                io::ErrorKind::Unsupported,
                "abstract sockets need linux",
            )));
        }
    } else {
        UnixStream::connect(name)?
    };

    stream.set_read_timeout(Some(read_timeout))?;
    Ok(Box::new(stream))
}

#[cfg(not(unix))]
fn connect_unix(_name: &str, _read_timeout: Duration) -> Result<Box<dyn Stream>, Error> {
    Err(Error::Io(io::Error::new(
        io::ErrorKind::Unsupported,
        "unix sockets need a unix host",
    )))
}

/// Read until the peer is idle or closes. Returns bytes and a truncation flag.
pub fn read_until_idle(
    stream: &mut dyn Stream,
    idle: Duration,
    max_bytes: usize,
) -> io::Result<(Vec<u8>, bool)> {
    stream.set_read_timeout(Some(idle))?;
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        if out.len() >= max_bytes {
            return Ok((out, true));
        }
        match stream.read(&mut buf) {
            Ok(0) => return Ok((out, false)),
            Ok(n) => {
                let take = n.min(max_bytes - out.len());
                out.extend_from_slice(&buf[..take]);
                if take < n {
                    return Ok((out, true));
                }
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err)
                if err.kind() == io::ErrorKind::WouldBlock
                    || err.kind() == io::ErrorKind::TimedOut =>
            {
                return Ok((out, false))
            }
            Err(err) => return Err(err),
        }
    }
}

/// Read for a fixed window, useful for streaming replies.
pub fn read_for(
    stream: &mut dyn Stream,
    total: Duration,
    slice: Duration,
    max_bytes: usize,
) -> io::Result<(Vec<u8>, bool)> {
    stream.set_read_timeout(Some(slice))?;
    let deadline = Instant::now() + total;
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        if Instant::now() >= deadline {
            return Ok((out, false));
        }
        if out.len() >= max_bytes {
            return Ok((out, true));
        }
        match stream.read(&mut buf) {
            Ok(0) => return Ok((out, false)),
            Ok(n) => {
                let take = n.min(max_bytes - out.len());
                out.extend_from_slice(&buf[..take]);
                if take < n {
                    return Ok((out, true));
                }
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err)
                if err.kind() == io::ErrorKind::WouldBlock
                    || err.kind() == io::ErrorKind::TimedOut =>
            {
                continue
            }
            Err(err) => return Err(err),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn tcp_roundtrip() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let server = thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut cmd = [0u8; 4];
            sock.read_exact(&mut cmd).unwrap();
            assert_eq!(cmd, [0x02, 0x00, 0x02, 0x01]);
            sock.write_all(&[0x01, 0x00, 0xcd, 0xab, 0, 0, 0, 0, 0, 0, 0, 0])
                .unwrap();
        });

        let mut client = connect(
            &Target::Tcp(addr.to_string()),
            Duration::from_millis(500),
            Duration::from_millis(500),
        )
        .unwrap();
        client.write_all(&[0x02, 0x00, 0x02, 0x01]).unwrap();
        let (data, truncated) =
            read_until_idle(client.as_mut(), Duration::from_millis(300), 1024).unwrap();

        assert!(!truncated);
        assert_eq!(data.len(), 12);
        assert_eq!(&data[..4], &[0x01, 0x00, 0xcd, 0xab]);
        server.join().unwrap();
    }

    #[test]
    fn idle_returns_empty() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (_sock, _) = listener.accept().unwrap();
            thread::sleep(Duration::from_millis(600));
        });

        let mut client = connect(
            &Target::Tcp(addr.to_string()),
            Duration::from_millis(500),
            Duration::from_millis(500),
        )
        .unwrap();
        let (data, truncated) =
            read_until_idle(client.as_mut(), Duration::from_millis(100), 1024).unwrap();
        assert!(data.is_empty());
        assert!(!truncated);
        server.join().unwrap();
    }
}
