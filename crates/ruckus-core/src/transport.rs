//! Bounded IRC framing over TCP, verified TLS, or injected asynchronous I/O.
//!
//! Independent body/tag budgets follow Lurk's `conn` package. Callers await
//! writes directly for backpressure; there are no background tasks or queues.

use std::{fmt, io, time::Duration};

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::time::{Instant, timeout, timeout_at};
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::{ClientConfig, RootCertStore, pki_types::ServerName};

/// Non-tag message budget, including CRLF.
pub const MAX_MESSAGE_BYTES: usize = 512;
/// Server tag-section budget, including `@` and the separating space.
pub const MAX_TAG_BYTES: usize = 8191;
const MAX_WIRE_BYTES: usize = MAX_MESSAGE_BYTES + MAX_TAG_BYTES;

/// Time limits for transport operations.
#[derive(Clone, Copy, Debug)]
pub struct TransportOptions {
    /// Combined DNS, TCP connection, and TLS handshake budget (default 30s).
    pub connect_timeout: Duration,
    /// Optional total time to receive a line; disabled by default.
    ///
    /// When enabled, trickling bytes or cancelling `read_line` does not extend
    /// the budget. Normally the client manages liveness with PING instead.
    pub read_timeout: Option<Duration>,
    /// Time to write and flush one line (default 30s).
    pub write_timeout: Duration,
    /// Graceful shutdown budget (default 2s).
    pub shutdown_timeout: Duration,
}

impl Default for TransportOptions {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(30),
            read_timeout: None,
            write_timeout: Duration::from_secs(30),
            shutdown_timeout: Duration::from_secs(2),
        }
    }
}

/// A transport failure. Error messages never include wire contents.
#[derive(Debug)]
pub enum TransportError {
    /// Underlying stream or TLS handshake failure.
    Io(io::Error),
    /// Host is not a valid TLS server name or IP address.
    InvalidServerName,
    /// An operation exceeded its time budget.
    Timeout,
    /// Message body or tag section exceeds its byte budget.
    LineTooLong,
    /// Missing CRLF, embedded CR/LF/NUL, missing tag separator, or invalid UTF-8.
    InvalidLine,
    /// The peer closed in the middle of a line.
    UnterminatedLine,
    /// Transport was shut down or failed, or a write was cancelled.
    Closed,
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Io(_) => "transport I/O failure",
            Self::InvalidServerName => "invalid TLS server name",
            Self::Timeout => "transport operation timed out",
            Self::LineTooLong => "IRC line exceeds byte budget",
            Self::InvalidLine => "invalid IRC wire line",
            Self::UnterminatedLine => "EOF in unterminated IRC line",
            Self::Closed => "transport is closed",
        })
    }
}

impl std::error::Error for TransportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for TransportError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<S: AsyncRead + AsyncWrite + Unpin + Send> Stream for S {}

/// A single owned, bounded IRC stream.
///
/// TLS always verifies the certificate chain and host against Mozilla roots.
/// Plain TCP is explicit (`tls = false`); callers enforce credential policy.
/// Injected streams carry no implied security guarantee.
///
/// Dropping this value drops its stream. `read_line` is cancellation-safe.
/// Cancelling a write poisons the transport: an unknown prefix may already have
/// reached the peer, so later I/O fails closed.
pub struct Transport {
    io: BufReader<Box<dyn Stream>>,
    partial: Vec<u8>,
    read_deadline: Option<Instant>,
    options: TransportOptions,
    closed: bool,
    eof: bool,
}

impl Transport {
    /// Connect using default time limits and, when requested, verified TLS.
    pub async fn connect(host: &str, port: u16, tls: bool) -> Result<Self, TransportError> {
        Self::connect_with_options(host, port, tls, TransportOptions::default()).await
    }

    /// Connect with explicit time limits. TLS cannot be configured insecurely.
    pub async fn connect_with_options(
        host: &str,
        port: u16,
        tls: bool,
        options: TransportOptions,
    ) -> Result<Self, TransportError> {
        let connect = async {
            let server_name = if tls {
                Some(
                    ServerName::try_from(host.to_owned())
                        .map_err(|_| TransportError::InvalidServerName)?,
                )
            } else {
                None
            };
            let tcp = TcpStream::connect((host, port)).await?;
            tcp.set_nodelay(true)?;
            if let Some(server_name) = server_name {
                let roots =
                    RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
                let stream = verified_tls(tcp, server_name, roots).await?;
                Ok(Self::from_stream_with_options(stream, options))
            } else {
                Ok(Self::from_stream_with_options(tcp, options))
            }
        };
        timeout(options.connect_timeout, connect)
            .await
            .map_err(|_| TransportError::Timeout)?
    }

    /// Own an established stream with default time limits.
    ///
    /// This is the injection seam for hermetic tests and custom connectors.
    pub fn from_stream<S>(stream: S) -> Self
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        Self::from_stream_with_options(stream, TransportOptions::default())
    }

    /// Own an established stream with explicit time limits.
    pub fn from_stream_with_options<S>(stream: S, options: TransportOptions) -> Self
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        Self {
            io: BufReader::with_capacity(1024, Box::new(stream)),
            partial: Vec::with_capacity(MAX_WIRE_BYTES),
            read_deadline: None,
            options,
            closed: false,
            eof: false,
        }
    }

    /// Read a CRLF-terminated UTF-8 line, excluding CRLF.
    ///
    /// Clean EOF is `Ok(None)`. Partial EOF, malformed framing, timeouts, and
    /// oversized input are terminal: later I/O returns `Closed`. Memory is
    /// bounded even without a terminator. Blank CRLF returns an empty string.
    /// Cancelling this method retains partial bytes and any configured deadline.
    pub async fn read_line(&mut self) -> Result<Option<String>, TransportError> {
        if self.closed {
            return Err(TransportError::Closed);
        }
        if self.eof {
            return Ok(None);
        }
        let result = self.read_inner().await;
        if result.is_err() {
            self.closed = true;
        }
        result
    }

    async fn read_inner(&mut self) -> Result<Option<String>, TransportError> {
        if self.read_deadline.is_none() {
            self.read_deadline = self
                .options
                .read_timeout
                .map(|limit| Instant::now() + limit);
        }
        loop {
            let available = match self.read_deadline {
                Some(deadline) => timeout_at(deadline, self.io.fill_buf())
                    .await
                    .map_err(|_| TransportError::Timeout)??,
                None => self.io.fill_buf().await?,
            };
            if available.is_empty() {
                self.eof = true;
                return if self.partial.is_empty() {
                    Ok(None)
                } else {
                    Err(TransportError::UnterminatedLine)
                };
            }
            let count = available
                .iter()
                .position(|&byte| byte == b'\n')
                .map_or(available.len(), |index| index + 1);
            if count > MAX_WIRE_BYTES - self.partial.len() {
                return Err(TransportError::LineTooLong);
            }
            self.partial.extend_from_slice(&available[..count]);
            self.io.consume(count);
            if self.partial.last() == Some(&b'\n') {
                let content = self
                    .partial
                    .strip_suffix(b"\r\n")
                    .ok_or(TransportError::InvalidLine)?;
                validate_content(content)?;
                let line = std::str::from_utf8(content)
                    .map_err(|_| TransportError::InvalidLine)?
                    .to_owned();
                self.partial.clear();
                self.read_deadline = None;
                return Ok(Some(line));
            }
            if self.partial.len() == MAX_WIRE_BYTES {
                return Err(TransportError::LineTooLong);
            }
        }
    }

    /// Write and flush a serialized wire line, including exactly one CRLF.
    ///
    /// Byte budgets and CR/LF/NUL injection are checked before I/O. Invalid caller
    /// input leaves the transport usable. I/O errors, timeouts, or cancellation
    /// make it unusable; never retry a potentially partially written line.
    pub async fn write_line(&mut self, wire: &str) -> Result<(), TransportError> {
        if self.closed {
            return Err(TransportError::Closed);
        }
        let content = wire
            .as_bytes()
            .strip_suffix(b"\r\n")
            .ok_or(TransportError::InvalidLine)?;
        validate_content(content)?;
        // Poison before awaiting, so cancellation cannot permit another command
        // to follow an unknown partial write.
        self.closed = true;
        timeout(self.options.write_timeout, async {
            self.io.get_mut().write_all(wire.as_bytes()).await?;
            self.io.get_mut().flush().await
        })
        .await
        .map_err(|_| TransportError::Timeout)??;
        self.closed = false;
        Ok(())
    }

    /// Shut down the write side, including TLS close-notify, within the budget.
    ///
    /// No further reads or writes are allowed. Drop the transport to release the
    /// socket completely. Calling shutdown again is safe.
    pub async fn shutdown(&mut self) -> Result<(), TransportError> {
        self.closed = true;
        timeout(self.options.shutdown_timeout, self.io.get_mut().shutdown())
            .await
            .map_err(|_| TransportError::Timeout)??;
        Ok(())
    }
}

// Production always passes Mozilla roots. Keeping this private seam separate
// lets tests exercise the actual handshake with a generated trust anchor,
// without exposing insecure verification switches or custom production roots.
async fn verified_tls<S>(
    stream: S,
    server_name: ServerName<'static>,
    roots: RootCertStore,
) -> io::Result<tokio_rustls::client::TlsStream<S>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    TlsConnector::from(std::sync::Arc::new(config))
        .connect(server_name, stream)
        .await
}

fn validate_content(content: &[u8]) -> Result<(), TransportError> {
    if content.iter().any(|byte| matches!(byte, b'\r' | b'\n' | 0)) {
        return Err(TransportError::InvalidLine);
    }
    let body = if content.first() == Some(&b'@') {
        let space = content
            .iter()
            .position(|&byte| byte == b' ')
            .ok_or(TransportError::InvalidLine)?;
        if space + 1 > MAX_TAG_BYTES {
            return Err(TransportError::LineTooLong);
        }
        &content[space + 1..]
    } else {
        content
    };
    if body.len() > MAX_MESSAGE_BYTES - 2 {
        return Err(TransportError::LineTooLong);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, duplex};

    async fn handshake_with_test_certificate(
        trust_certificate: bool,
        hostname: &'static str,
    ) -> io::Result<()> {
        use tokio_rustls::rustls::{ServerConfig, pki_types::PrivatePkcs8KeyDer};

        let certificate =
            rcgen::generate_simple_self_signed(vec!["irc.example.test".to_owned()]).unwrap();
        let cert_der = certificate.cert.der().clone();
        let key = PrivatePkcs8KeyDer::from(certificate.key_pair.serialize_der());
        let server_config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert_der.clone()], key.into())
            .unwrap();
        let mut roots = if trust_certificate {
            RootCertStore::empty()
        } else {
            RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned())
        };
        if trust_certificate {
            roots.add(cert_der).unwrap();
        }
        let (client, server) = duplex(16 * 1024);
        let server = tokio::spawn(async move {
            let acceptor = tokio_rustls::TlsAcceptor::from(std::sync::Arc::new(server_config));
            match acceptor.accept(server).await {
                Ok(stream) => {
                    let mut transport = Transport::from_stream(stream);
                    transport.write_line("PING :verified\r\n").await.unwrap();
                    transport.shutdown().await.unwrap();
                }
                // Certificate rejection sends a fatal alert to the test peer.
                Err(error) => assert_eq!(error.kind(), io::ErrorKind::InvalidData),
            }
        });
        let result =
            match verified_tls(client, ServerName::try_from(hostname).unwrap(), roots).await {
                Ok(stream) => {
                    let mut transport = Transport::from_stream(stream);
                    assert_eq!(
                        transport.read_line().await.unwrap().as_deref(),
                        Some("PING :verified")
                    );
                    Ok(())
                }
                Err(error) => Err(error),
            };
        server.await.unwrap();
        result
    }

    #[tokio::test]
    async fn tls_accepts_trusted_certificate_with_matching_hostname() {
        timeout(
            Duration::from_secs(5),
            handshake_with_test_certificate(true, "irc.example.test"),
        )
        .await
        .unwrap()
        .unwrap();
    }

    #[tokio::test]
    async fn tls_rejects_untrusted_certificate() {
        let error = timeout(
            Duration::from_secs(5),
            handshake_with_test_certificate(false, "irc.example.test"),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert!(matches!(
            error
                .get_ref()
                .and_then(|source| source.downcast_ref::<tokio_rustls::rustls::Error>()),
            Some(tokio_rustls::rustls::Error::InvalidCertificate(_))
        ));
    }

    #[tokio::test]
    async fn tls_rejects_hostname_mismatch_even_with_trusted_certificate() {
        let error = timeout(
            Duration::from_secs(5),
            handshake_with_test_certificate(true, "different.example.test"),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert!(matches!(
            error
                .get_ref()
                .and_then(|source| source.downcast_ref::<tokio_rustls::rustls::Error>()),
            Some(tokio_rustls::rustls::Error::InvalidCertificate(_))
        ));
    }

    #[tokio::test]
    async fn invalid_tls_name_is_rejected_before_dialing() {
        assert!(matches!(
            Transport::connect("not a valid host", 6697, true).await,
            Err(TransportError::InvalidServerName)
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn default_reads_allow_idle_networks() {
        let (stream, mut peer) = duplex(64);
        let mut transport = Transport::from_stream(stream);
        assert!(
            timeout(Duration::from_secs(3600), transport.read_line())
                .await
                .is_err()
        );
        peer.write_all(b"PING\r\n").await.unwrap();
        assert_eq!(
            transport.read_line().await.unwrap().as_deref(),
            Some("PING")
        );
    }

    #[tokio::test]
    async fn stream_write_errors_are_terminal() {
        let (stream, peer) = duplex(64);
        let mut transport = Transport::from_stream(stream);
        drop(peer);
        assert!(matches!(
            transport.write_line("PING\r\n").await,
            Err(TransportError::Io(_))
        ));
        assert!(matches!(
            transport.read_line().await,
            Err(TransportError::Closed)
        ));
    }

    #[tokio::test]
    async fn fragmented_lines_and_clean_eof() {
        let (stream, mut peer) = duplex(32);
        let mut transport = Transport::from_stream(stream);
        let writer = tokio::spawn(async move {
            for chunk in [b"PING :one\r".as_slice(), b"\nPONG :two\r\n\r\n"] {
                peer.write_all(chunk).await.unwrap();
                tokio::task::yield_now().await;
            }
        });
        assert_eq!(
            transport.read_line().await.unwrap().as_deref(),
            Some("PING :one")
        );
        assert_eq!(
            transport.read_line().await.unwrap().as_deref(),
            Some("PONG :two")
        );
        assert_eq!(transport.read_line().await.unwrap().as_deref(), Some(""));
        assert_eq!(transport.read_line().await.unwrap(), None);
        assert_eq!(transport.read_line().await.unwrap(), None);
        writer.await.unwrap();
    }

    async fn read_bytes(bytes: Vec<u8>) -> Result<Option<String>, TransportError> {
        let (stream, mut peer) = duplex(MAX_WIRE_BYTES * 2);
        peer.write_all(&bytes).await.unwrap();
        drop(peer);
        Transport::from_stream(stream).read_line().await
    }

    #[tokio::test]
    async fn independent_byte_budgets() {
        let body = "x".repeat(MAX_MESSAGE_BYTES - 2);
        let tags = format!("@{} ", "t".repeat(MAX_TAG_BYTES - 2));
        let maximal = format!("{tags}{body}\r\n");
        assert_eq!(
            read_bytes(maximal.into_bytes())
                .await
                .unwrap()
                .unwrap()
                .len(),
            MAX_WIRE_BYTES - 2
        );
        for wire in [
            format!("{body}x\r\n"),
            format!("{tags}x{body}\r\n"),
            format!("@{} {body}\r\n", "t".repeat(MAX_TAG_BYTES - 1)),
        ] {
            assert!(matches!(
                read_bytes(wire.into_bytes()).await,
                Err(TransportError::LineTooLong)
            ));
        }
        assert!(matches!(
            read_bytes(format!("{}\r\n", "é".repeat(256)).into_bytes()).await,
            Err(TransportError::LineTooLong)
        ));
    }

    #[tokio::test]
    async fn rejects_malformed_and_incomplete_input() {
        for wire in [
            b"PING\n".to_vec(),
            b"PI\rNG\r\n".to_vec(),
            b"PI\0NG\r\n".to_vec(),
            b"\xff\r\n".to_vec(),
            b"@tag\r\n".to_vec(),
        ] {
            assert!(matches!(
                read_bytes(wire).await,
                Err(TransportError::InvalidLine)
            ));
        }
        for wire in [b"PING".to_vec(), b"PING\r".to_vec()] {
            assert!(matches!(
                read_bytes(wire).await,
                Err(TransportError::UnterminatedLine)
            ));
        }
    }

    #[tokio::test]
    async fn unterminated_hostile_stream_is_bounded_and_terminal() {
        let (stream, mut peer) = duplex(MAX_WIRE_BYTES * 2);
        peer.write_all(&vec![b'x'; MAX_WIRE_BYTES + 1])
            .await
            .unwrap();
        let mut transport = Transport::from_stream(stream);
        assert!(matches!(
            transport.read_line().await,
            Err(TransportError::LineTooLong)
        ));
        assert!(transport.partial.len() <= MAX_WIRE_BYTES);
        assert_eq!(transport.partial.capacity(), MAX_WIRE_BYTES);
        assert!(matches!(
            transport.read_line().await,
            Err(TransportError::Closed)
        ));
    }

    #[tokio::test]
    async fn writes_exact_wire_and_rejects_injection_without_sending() {
        let (stream, mut peer) = duplex(1024);
        let mut transport = Transport::from_stream(stream);
        for wire in ["PING", "PING\n", "PING\r\nQUIT\r\n", "PI\0NG\r\n"] {
            assert!(matches!(
                transport.write_line(wire).await,
                Err(TransportError::InvalidLine)
            ));
        }
        assert!(matches!(
            transport
                .write_line(&format!("{}\r\n", "x".repeat(511)))
                .await,
            Err(TransportError::LineTooLong)
        ));
        transport.write_line("PING :safe\r\n").await.unwrap();
        transport.shutdown().await.unwrap();
        let mut bytes = Vec::new();
        peer.read_to_end(&mut bytes).await.unwrap();
        assert_eq!(bytes, b"PING :safe\r\n");
        assert!(matches!(
            transport.write_line("PING\r\n").await,
            Err(TransportError::Closed)
        ));
        assert!(matches!(
            transport.read_line().await,
            Err(TransportError::Closed)
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn read_cancellation_preserves_fragments_and_deadline() {
        let (stream, mut peer) = duplex(64);
        let options = TransportOptions {
            read_timeout: Some(Duration::from_secs(300)),
            ..TransportOptions::default()
        };
        let mut transport = Transport::from_stream_with_options(stream, options);
        peer.write_all(b"PING :frag").await.unwrap();
        assert!(
            timeout(Duration::from_secs(1), transport.read_line())
                .await
                .is_err()
        );
        peer.write_all(b"ment\r\n").await.unwrap();
        assert_eq!(
            transport.read_line().await.unwrap().as_deref(),
            Some("PING :fragment")
        );
        assert!(
            timeout(Duration::from_secs(299), transport.read_line())
                .await
                .is_err()
        );
        tokio::time::advance(Duration::from_secs(2)).await;
        assert!(matches!(
            transport.read_line().await,
            Err(TransportError::Timeout)
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn blocked_writes_time_out_or_fail_closed_on_cancellation() {
        let (stream, _peer) = duplex(1);
        let mut transport = Transport::from_stream(stream);
        assert!(matches!(
            transport.write_line("PING\r\n").await,
            Err(TransportError::Timeout)
        ));
        assert!(matches!(
            transport.write_line("PING\r\n").await,
            Err(TransportError::Closed)
        ));
        let (stream, _peer) = duplex(1);
        let mut transport = Transport::from_stream(stream);
        assert!(
            timeout(Duration::from_secs(1), transport.write_line("PING\r\n"))
                .await
                .is_err()
        );
        assert!(matches!(
            transport.write_line("PING\r\n").await,
            Err(TransportError::Closed)
        ));
    }
}
