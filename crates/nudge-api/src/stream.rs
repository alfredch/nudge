use std::collections::VecDeque;

use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::http::header::USER_AGENT;
use tokio_tungstenite::tungstenite::{self, Message as WsMessage};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::error::{Error, Result};
use crate::types::{DeviceId, Secret};

pub const DEFAULT_PUSH_URL: &str = "wss://client.pushover.net/push";

/// A single-byte frame sent by the push server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frame {
    /// `#`: keep-alive, no reaction needed.
    Keepalive,
    /// `!`: new messages are available; fetch them over HTTP.
    NewMessages,
    /// `R`: the server asks to drop the connection and reconnect.
    Reload,
    /// `E`: permanent error, e.g. failed login. Do not reconnect automatically.
    Error,
    /// `A`: the device logged in from another session. Do not reconnect automatically.
    LoggedInElsewhere,
    /// Any other byte. Should be logged and ignored.
    Unknown(u8),
}

impl Frame {
    /// Parses one frame byte. Whitespace yields `None`.
    pub fn parse(byte: u8) -> Option<Frame> {
        Some(match byte {
            b'#' => Frame::Keepalive,
            b'!' => Frame::NewMessages,
            b'R' => Frame::Reload,
            b'E' => Frame::Error,
            b'A' => Frame::LoggedInElsewhere,
            b if b.is_ascii_whitespace() => return None,
            other => Frame::Unknown(other),
        })
    }

    /// Parses all frames in one WebSocket message.
    pub fn parse_all(bytes: &[u8]) -> impl Iterator<Item = Frame> + '_ {
        bytes.iter().copied().filter_map(Frame::parse)
    }

    /// Whether the client must stop and not reconnect on its own.
    pub fn is_terminal(self) -> bool {
        matches!(self, Frame::Error | Frame::LoggedInElsewhere)
    }
}

/// Logged-in WebSocket connection to the push server.
///
/// The stream does not reconnect and has no keep-alive watchdog; the caller
/// wraps [`PushStream::next_frame`] in a timeout and reconnects with backoff.
pub struct PushStream {
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
    pending: VecDeque<Frame>,
}

impl PushStream {
    /// Connects to `url` and sends the login frame.
    ///
    /// A failed login is not reported here: the server answers with an
    /// [`Frame::Error`] and closes the connection.
    pub async fn connect(
        url: &str,
        user_agent: &str,
        device: &DeviceId,
        secret: &Secret,
    ) -> Result<Self> {
        let mut request = url.into_client_request()?;
        let ua = HeaderValue::from_str(user_agent)
            .map_err(|e| tungstenite::Error::HttpFormat(e.into()))?;
        request.headers_mut().insert(USER_AGENT, ua);

        let (mut ws, _response) = tokio_tungstenite::connect_async(request).await?;
        let login = format!("login:{}:{}\n", device.as_str(), secret.expose());
        ws.send(WsMessage::text(login)).await?;
        Ok(Self {
            ws,
            pending: VecDeque::new(),
        })
    }

    /// Waits for the next frame. `Ok(None)` means the server closed the connection.
    pub async fn next_frame(&mut self) -> Result<Option<Frame>> {
        loop {
            if let Some(frame) = self.pending.pop_front() {
                return Ok(Some(frame));
            }
            let msg = match self.ws.next().await {
                None => return Ok(None),
                Some(Err(
                    tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed,
                )) => {
                    return Ok(None);
                }
                Some(Err(e)) => return Err(Error::from(e)),
                Some(Ok(msg)) => msg,
            };
            match msg {
                WsMessage::Binary(bytes) => self.pending.extend(Frame::parse_all(&bytes)),
                WsMessage::Text(text) => self.pending.extend(Frame::parse_all(text.as_bytes())),
                WsMessage::Close(_) => return Ok(None),
                // Pings are answered by tungstenite while reading.
                WsMessage::Ping(_) | WsMessage::Pong(_) | WsMessage::Frame(_) => {}
            }
        }
    }

    /// Closes the connection gracefully.
    pub async fn close(mut self) -> Result<()> {
        match self.ws.close(None).await {
            Ok(())
            | Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                Ok(())
            }
            Err(e) => Err(e.into()),
        }
    }
}

impl std::fmt::Debug for PushStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PushStream")
            .field("pending", &self.pending)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_frames() {
        assert_eq!(Frame::parse(b'#'), Some(Frame::Keepalive));
        assert_eq!(Frame::parse(b'!'), Some(Frame::NewMessages));
        assert_eq!(Frame::parse(b'R'), Some(Frame::Reload));
        assert_eq!(Frame::parse(b'E'), Some(Frame::Error));
        assert_eq!(Frame::parse(b'A'), Some(Frame::LoggedInElsewhere));
    }

    #[test]
    fn ignores_whitespace_and_keeps_unknown() {
        assert_eq!(Frame::parse(b'\n'), None);
        assert_eq!(Frame::parse(b' '), None);
        assert_eq!(Frame::parse(b'x'), Some(Frame::Unknown(b'x')));
    }

    #[test]
    fn parses_multiple_frames_per_message() {
        let frames: Vec<_> = Frame::parse_all(b"#!\n#").collect();
        assert_eq!(
            frames,
            [Frame::Keepalive, Frame::NewMessages, Frame::Keepalive]
        );
    }

    #[test]
    fn terminal_frames() {
        assert!(Frame::Error.is_terminal());
        assert!(Frame::LoggedInElsewhere.is_terminal());
        assert!(!Frame::Reload.is_terminal());
        assert!(!Frame::Keepalive.is_terminal());
    }
}
