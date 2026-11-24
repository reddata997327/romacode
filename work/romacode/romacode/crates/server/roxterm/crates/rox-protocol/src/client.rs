//! Minimal RPC client over the daemon socket (shared by CLI + runbooks).
//!
//! Transport: Unix domain socket on Unix, TCP loopback on Windows — the
//! framing (length-prefixed postcard) is identical on both.

use std::io::{Read, Write};
use std::time::Duration;

use crate::framing::{encode_frame, try_decode_frame};
use crate::{Request, Response};

/// Abstracted IPC stream: UDS on Unix, TCP loopback on Windows.
pub trait IpcStream: Read + Write + Send {
    fn try_dup(&self) -> std::io::Result<Box<dyn IpcStream>>;
    fn set_long_read_timeout(&self) -> std::io::Result<()>;
    /// Set the read timeout back to a finite value (per-RPC deadline).
    fn set_read_timeout(&self, d: Duration) -> std::io::Result<()>;
}

#[cfg(unix)]
impl IpcStream for std::os::unix::net::UnixStream {
    fn try_dup(&self) -> std::io::Result<Box<dyn IpcStream>> {
        let s = self.try_clone()?;
        Ok(Box::new(s))
    }
    fn set_long_read_timeout(&self) -> std::io::Result<()> {
        self.set_read_timeout(Some(Duration::from_secs(86400)))
    }
    fn set_read_timeout(&self, d: Duration) -> std::io::Result<()> {
        self.set_read_timeout(Some(d))
    }
}

#[cfg(windows)]
impl IpcStream for std::net::TcpStream {
    fn try_dup(&self) -> std::io::Result<Box<dyn IpcStream>> {
        let s = self.try_clone()?;
        Ok(Box::new(s))
    }
    fn set_long_read_timeout(&self) -> std::io::Result<()> {
        self.set_read_timeout(Some(Duration::from_secs(86400)))
    }
    fn set_read_timeout(&self, d: Duration) -> std::io::Result<()> {
        self.set_read_timeout(Some(d))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("daemon unreachable: {0}")]
    Unreachable(String),
    #[error("io: {0}")]
    Io(String),
    #[error("wire: {0}")]
    Wire(#[from] crate::WireError),
    #[error("daemon error [{code}]: {message}")]
    Daemon { code: i32, message: String },
    /// Per-RPC deadline elapsed. The request may still be running on the
    /// daemon — this is a client-side abort, not a daemon-side cancel.
    #[error("rpc timeout after {0:?}")]
    Timeout(Duration),
}

/// Default per-RPC deadline. Generous enough for the slowest daemon op
/// (journal export of a multi-MB chain) but bounded so a wedged daemon
/// can never hang the CLI forever. Long-running RPCs (Run, Wait, Stream*)
/// override this with their own `timeout_ms`.
pub const DEFAULT_RPC_TIMEOUT: Duration = Duration::from_secs(60);

/// Pick a client-side RPC deadline that always exceeds the daemon-side one.
/// The daemon enforces its own timeout (e.g. `WaitSpec.timeout_ms`,
/// `RunSpec.timeout_ms`) and returns a typed reply; the client deadline just
/// guards against a wedged daemon. We add 5s of slack so the daemon's typed
/// timeout reply arrives before the client gives up.
///
/// Shared between the `roxterm` CLI and the runbook executor so both paths
/// get the same per-RPC deadline without duplicating the match.
pub fn rpc_timeout_for(req: &crate::Request) -> Duration {
    use crate::Request::*;
    let slack = |ms: u64| Duration::from_millis(ms.saturating_add(5_000));
    match req {
        Run { spec, .. } => slack(spec.timeout_ms.unwrap_or(120_000)),
        Wait { spec, .. } => slack(spec.timeout_ms),
        CaptureWait { timeout_ms, .. } => slack(*timeout_ms),
        WsRecv { timeout_ms, .. } => slack(*timeout_ms),
        WsRun { spec } => slack(spec.timeout_ms),
        WsOpen { spec } => slack(spec.timeout_ms),
        _ => DEFAULT_RPC_TIMEOUT,
    }
}

pub struct Connection {
    stream: Box<dyn IpcStream>,
}

impl Connection {
    pub fn connect(endpoint: &crate::Endpoint) -> Result<Self, ClientError> {
        let stream: Box<dyn IpcStream> = match endpoint {
            #[cfg(unix)]
            crate::Endpoint::Uds(path) => {
                let s = std::os::unix::net::UnixStream::connect(path)
                    .map_err(|e| ClientError::Unreachable(format!("{}: {e}", path.display())))?;
                Box::new(s)
            }
            #[cfg(windows)]
            crate::Endpoint::Tcp(addr) => {
                let s = std::net::TcpStream::connect(addr.as_str())
                    .map_err(|e| ClientError::Unreachable(format!("{addr}: {e}")))?;
                Box::new(s)
            }
            #[cfg(not(any(unix, windows)))]
            _ => return Err(ClientError::Unreachable("unsupported platform".into())),
        };
        // Default deadline for non-streaming RPCs. Streaming RPCs
        // (Stream/StreamFrames) call open_push_channel which is one-way; the
        // subsequent read_push calls don't need a deadline (they block by
        // design), so the long timeout is re-applied in read_push.
        stream
            .set_read_timeout(DEFAULT_RPC_TIMEOUT)
            .map_err(|e| ClientError::Io(e.to_string()))?;
        Ok(Connection { stream })
    }

    /// Call the daemon with the default 60s RPC deadline.
    pub fn call(&mut self, req: Request) -> Result<Response, ClientError> {
        self.call_with_timeout(req, DEFAULT_RPC_TIMEOUT)
    }

    /// Call the daemon with an explicit deadline. Use this for `Run`/`Wait`
    /// (which can legitimately run for minutes) and for `Stream`/
    /// `StreamFrames` registration (the registration itself is fast). The
    /// deadline applies to the reply only — push channels switch to
    /// blocking reads via [`Connection::read_push`].
    pub fn call_with_timeout(
        &mut self,
        req: Request,
        timeout: Duration,
    ) -> Result<Response, ClientError> {
        // Apply the deadline to the read side; write side stays untimed
        // (writes on UDS/TCP loopback are effectively instantaneous, but a
        // wedged daemon could stall the write — bound it to the same value).
        self.stream
            .set_read_timeout(timeout)
            .map_err(|e| ClientError::Io(e.to_string()))?;
        let frame = encode_frame(&req)?;
        self.stream
            .write_all(&frame)
            .map_err(|e| ClientError::Io(e.to_string()))?;
        self.stream
            .flush()
            .map_err(|e| ClientError::Io(e.to_string()))?;
        let mut buf = Vec::with_capacity(8192);
        let mut chunk = [0u8; 16384];
        loop {
            if let Some((resp, n)) = try_decode_frame::<Response>(&buf)? {
                buf.drain(..n);
                // Restore the default deadline for the next call.
                let _ = self.stream.set_read_timeout(DEFAULT_RPC_TIMEOUT);
                return match resp {
                    Response::Error(e) => Err(ClientError::Daemon {
                        code: e.code,
                        message: e.message,
                    }),
                    r => Ok(r),
                };
            }
            let n = match self.stream.read(&mut chunk) {
                Ok(n) => n,
                Err(e)
                    if e.kind() == std::io::ErrorKind::TimedOut
                        || e.kind() == std::io::ErrorKind::WouldBlock =>
                {
                    let _ = self.stream.set_read_timeout(DEFAULT_RPC_TIMEOUT);
                    return Err(ClientError::Timeout(timeout));
                }
                Err(e) => {
                    let _ = self.stream.set_read_timeout(DEFAULT_RPC_TIMEOUT);
                    return Err(ClientError::Io(e.to_string()));
                }
            };
            if n == 0 {
                let _ = self.stream.set_read_timeout(DEFAULT_RPC_TIMEOUT);
                return Err(ClientError::Io("daemon closed connection".into()));
            }
            buf.extend_from_slice(&chunk[..n]);
        }
    }

    /// 1.0.0: open a one-way push feed. Sends `req` (Stream / StreamFrames)
    /// WITHOUT waiting for a reply — after registering, the daemon stops
    /// dispatching on this connection and pushes [`Response`] frames instead.
    /// An unknown target yields a normal in-band `Response::Error` as the
    /// first frame read by [`Connection::read_push`].
    pub fn open_push_channel(&mut self, req: Request) -> Result<(), ClientError> {
        // Push channels are read with no deadline (they block by design).
        self.stream
            .set_long_read_timeout()
            .map_err(|e| ClientError::Io(e.to_string()))?;
        let frame = encode_frame(&req)?;
        self.stream
            .write_all(&frame)
            .map_err(|e| ClientError::Io(e.to_string()))?;
        self.stream
            .flush()
            .map_err(|e| ClientError::Io(e.to_string()))
    }

    /// Read one pushed frame (blocks until it arrives). `Ok(None)` on clean
    /// daemon disconnect. Unlike [`Connection::call`] this does NOT convert
    /// `Response::Error` into `Err` — on a feed, the error reply IS the
    /// first frame (e.g. unknown connection name).
    pub fn read_push(&mut self) -> Result<Option<Response>, ClientError> {
        let mut buf = Vec::with_capacity(8192);
        let mut chunk = [0u8; 16384];
        loop {
            if let Some((resp, n)) = try_decode_frame::<Response>(&buf)? {
                buf.drain(..n);
                return Ok(Some(resp));
            }
            let n = self
                .stream
                .read(&mut chunk)
                .map_err(|e| ClientError::Io(e.to_string()))?;
            if n == 0 {
                return Ok(None);
            }
            buf.extend_from_slice(&chunk[..n]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_error_display() {
        let e = ClientError::Daemon {
            code: 3,
            message: "assert failed".into(),
        };
        assert_eq!(e.to_string(), "daemon error [3]: assert failed");
    }
}
