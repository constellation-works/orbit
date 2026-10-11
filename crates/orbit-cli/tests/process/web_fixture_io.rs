//! Dashboard fixture IO shares one deadline across connect, write and read.

use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::{Duration, Instant};

pub(crate) const OPERATION_TIMEOUT: Duration = orbit_common::test_env::FIXTURE_STEP_DEADLINE;

pub(crate) struct DeadlineStream {
    stream: TcpStream,
    address: SocketAddr,
    deadline: Instant,
    timeout: Duration,
}

impl DeadlineStream {
    pub(crate) fn connect(port: u16, timeout: Duration) -> io::Result<Self> {
        let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let deadline = Instant::now() + timeout;
        let stream = TcpStream::connect_timeout(&address, timeout).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "connect to {address} failed (fixture operation deadline {timeout:?}): {error}"
                ),
            )
        })?;
        Ok(Self {
            stream,
            address,
            deadline,
            timeout,
        })
    }

    fn remaining(&self, stage: &str) -> io::Result<Duration> {
        self.deadline
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| self.diagnostic(stage, io::ErrorKind::TimedOut.into()))
    }

    fn diagnostic(&self, stage: &str, error: io::Error) -> io::Error {
        io::Error::new(
            error.kind(),
            format!(
                "{stage} on {} failed (fixture operation deadline {:?}): {error}",
                self.address, self.timeout
            ),
        )
    }
}

impl Read for DeadlineStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        // read_to_string can perform many reads. Renew only the socket's
        // remaining budget, never the operation deadline, even after progress.
        let timeout = self.remaining("response read")?;
        self.stream
            .set_read_timeout(Some(timeout))
            .and_then(|()| self.stream.read(buffer))
            .map_err(|error| self.diagnostic("response read", error))
    }
}

impl Write for DeadlineStream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let timeout = self.remaining("request write")?;
        self.stream
            .set_write_timeout(Some(timeout))
            .and_then(|()| self.stream.write(buffer))
            .map_err(|error| self.diagnostic("request write", error))
    }

    fn flush(&mut self) -> io::Result<()> {
        self.remaining("request flush")?;
        self.stream
            .flush()
            .map_err(|error| self.diagnostic("request flush", error))
    }
}

pub(crate) fn http_get(port: u16, path: &str) -> String {
    http_get_with_timeout(port, path, OPERATION_TIMEOUT)
}

pub(crate) fn http_get_with_timeout(port: u16, path: &str, timeout: Duration) -> String {
    let mut stream = DeadlineStream::connect(port, timeout).expect("connect HTTP fixture");
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .expect("write HTTP fixture request");
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("read HTTP fixture response");
    response
}
