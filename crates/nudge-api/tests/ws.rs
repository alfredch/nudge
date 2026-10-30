#![allow(clippy::unwrap_used, clippy::expect_used, clippy::result_large_err)]

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use nudge_api::{DeviceId, Frame, PushStream, Secret};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};

const UA: &str = "nudge-test/0.0.0 (Linux; unofficial Pushover Open Client)";

/// What the server should do after receiving the login frame.
#[derive(Clone, Copy)]
enum End {
    /// Send a close frame.
    Close,
    /// Drop the TCP connection without a close handshake.
    Drop,
    /// Stay silent until the client goes away.
    Hang,
}

/// Starts a one-shot push server. It returns the User-Agent and login line it received.
async fn serve(
    frames: &'static [&'static [u8]],
    end: End,
) -> (String, JoinHandle<(String, String)>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/push", listener.local_addr().unwrap());

    let handle = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut user_agent = String::new();
        let mut ws = tokio_tungstenite::accept_hdr_async(tcp, |req: &Request, resp: Response| {
            user_agent = req
                .headers()
                .get("user-agent")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_owned();
            Ok(resp)
        })
        .await
        .unwrap();

        let login = match ws.next().await {
            Some(Ok(Message::Text(t))) => t.to_string(),
            other => panic!("expected login frame, got {other:?}"),
        };
        for frame in frames {
            ws.send(Message::binary(frame.to_vec())).await.unwrap();
        }
        match end {
            End::Close => {
                let _ = ws.close(None).await;
            }
            End::Drop => drop(ws),
            End::Hang => while ws.next().await.is_some() {},
        }
        (user_agent, login)
    });
    (url, handle)
}

async fn connect(url: &str) -> PushStream {
    PushStream::connect(url, UA, &DeviceId::new("dev42"), &Secret::new("s3cr3t"))
        .await
        .unwrap()
}

async fn collect(stream: &mut PushStream) -> Vec<Frame> {
    let mut frames = Vec::new();
    while let Some(frame) = tokio::time::timeout(Duration::from_secs(5), stream.next_frame())
        .await
        .expect("stream hung")
        .unwrap()
    {
        frames.push(frame);
    }
    frames
}

#[tokio::test]
async fn login_frame_and_user_agent() {
    let (url, server) = serve(&[b"#"], End::Close).await;
    let mut stream = connect(&url).await;
    assert_eq!(collect(&mut stream).await, [Frame::Keepalive]);

    let (user_agent, login) = server.await.unwrap();
    assert_eq!(user_agent, UA);
    assert_eq!(login, "login:dev42:s3cr3t\n");
}

#[tokio::test]
async fn frame_sequence_until_close() {
    let (url, server) = serve(&[b"#", b"!", b"#!", b"x", b"R"], End::Close).await;
    let mut stream = connect(&url).await;
    assert_eq!(
        collect(&mut stream).await,
        [
            Frame::Keepalive,
            Frame::NewMessages,
            Frame::Keepalive,
            Frame::NewMessages,
            Frame::Unknown(b'x'),
            Frame::Reload,
        ]
    );
    server.await.unwrap();
}

#[tokio::test]
async fn error_frame_then_close() {
    let (url, server) = serve(&[b"E"], End::Close).await;
    let mut stream = connect(&url).await;
    let frames = collect(&mut stream).await;
    assert_eq!(frames, [Frame::Error]);
    assert!(frames[0].is_terminal());
    server.await.unwrap();
}

#[tokio::test]
async fn logged_in_elsewhere() {
    let (url, server) = serve(&[b"A"], End::Close).await;
    let mut stream = connect(&url).await;
    assert_eq!(collect(&mut stream).await, [Frame::LoggedInElsewhere]);
    server.await.unwrap();
}

#[tokio::test]
async fn abrupt_disconnect_ends_stream() {
    let (url, server) = serve(&[b"#"], End::Drop).await;
    let mut stream = connect(&url).await;
    assert_eq!(stream.next_frame().await.unwrap(), Some(Frame::Keepalive));
    // Depending on timing the reset surfaces as end of stream or as an error,
    // but never as a frame and never as a hang.
    let next = tokio::time::timeout(Duration::from_secs(5), stream.next_frame())
        .await
        .expect("stream hung");
    assert!(matches!(next, Ok(None) | Err(_)), "{next:?}");
    server.await.unwrap();
}

#[tokio::test]
async fn missing_keepalive_is_detectable_with_timeout() {
    let (url, server) = serve(&[], End::Hang).await;
    let mut stream = connect(&url).await;
    let waited = tokio::time::timeout(Duration::from_millis(200), stream.next_frame()).await;
    assert!(waited.is_err(), "expected timeout, got {waited:?}");
    stream.close().await.unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn connect_refused() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/push", listener.local_addr().unwrap());
    drop(listener);
    let result =
        PushStream::connect(&url, UA, &DeviceId::new("dev42"), &Secret::new("s3cr3t")).await;
    assert!(result.is_err());
}
