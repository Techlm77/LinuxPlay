//! A byte stream over a WebSocket: what lets the signalling connection run
//! through the rendezvous server exactly as it runs over TCP.
//!
//! Each write becomes one binary message; reads hand out the bytes of the
//! messages as they arrive. The TLS session on top neither knows nor cares
//! where the message boundaries fall.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll, ready};

use futures_util::{Sink, Stream};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::{Bytes, Error as WsError, Message};

/// Largest binary message written. The rendezvous server refuses larger ones.
const MAX_MESSAGE: usize = 64 << 10;

pub struct WsIo<S> {
    ws: WebSocketStream<S>,
    /// Bytes of the message being read that haven't been handed out yet.
    pending: Bytes,
}

impl<S> WsIo<S> {
    pub fn new(ws: WebSocketStream<S>) -> Self {
        Self { ws, pending: Bytes::new() }
    }
}

fn io_error(e: WsError) -> io::Error {
    match e {
        WsError::Io(e) => e,
        other => io::Error::other(other),
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> AsyncRead for WsIo<S> {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        while self.pending.is_empty() {
            match ready!(Pin::new(&mut self.ws).poll_next(cx)) {
                Some(Ok(Message::Binary(bytes))) => self.pending = bytes,
                // Keepalives; the socket answers pings by itself.
                Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => {}
                Some(Ok(Message::Text(_))) => return Poll::Ready(Err(io::Error::other("unexpected text in a tunnel"))),
                // End of stream.
                Some(Ok(Message::Close(_))) | None => return Poll::Ready(Ok(())),
                Some(Err(WsError::ConnectionClosed | WsError::AlreadyClosed)) => return Poll::Ready(Ok(())),
                Some(Err(e)) => return Poll::Ready(Err(io_error(e))),
            }
        }
        let n = self.pending.len().min(buf.remaining());
        buf.put_slice(&self.pending.split_to(n));
        Poll::Ready(Ok(()))
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> AsyncWrite for WsIo<S> {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, data: &[u8]) -> Poll<io::Result<usize>> {
        ready!(Pin::new(&mut self.ws).poll_ready(cx)).map_err(io_error)?;
        let n = data.len().min(MAX_MESSAGE);
        Pin::new(&mut self.ws).start_send(Message::Binary(Bytes::copy_from_slice(&data[..n]))).map_err(io_error)?;
        Poll::Ready(Ok(n))
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.ws).poll_flush(cx).map_err(io_error)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match ready!(Pin::new(&mut self.ws).poll_close(cx)) {
            Ok(()) | Err(WsError::ConnectionClosed | WsError::AlreadyClosed) => Poll::Ready(Ok(())),
            Err(e) => Poll::Ready(Err(io_error(e))),
        }
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_tungstenite::tungstenite::protocol::Role;

    use super::*;

    async fn pair() -> (WsIo<tokio::io::DuplexStream>, WsIo<tokio::io::DuplexStream>) {
        let (a, b) = tokio::io::duplex(1 << 16);
        let client = WebSocketStream::from_raw_socket(a, Role::Client, None).await;
        let server = WebSocketStream::from_raw_socket(b, Role::Server, None).await;
        (WsIo::new(client), WsIo::new(server))
    }

    #[tokio::test]
    async fn bytes_cross_in_order_whatever_the_message_sizes() {
        let (mut a, mut b) = pair().await;
        let sent: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        let expected = sent.clone();
        let writer = tokio::spawn(async move {
            // Uneven chunks, some larger than one message may be.
            for chunk in sent.chunks(70_001) {
                a.write_all(chunk).await.unwrap();
            }
            a.shutdown().await.unwrap();
        });
        let mut got = Vec::new();
        // Small reads, so messages are handed out piecemeal.
        let mut piece = [0u8; 1000];
        loop {
            let n = b.read(&mut piece).await.unwrap();
            if n == 0 {
                break;
            }
            got.extend_from_slice(&piece[..n]);
        }
        writer.await.unwrap();
        assert_eq!(got, expected);
    }

    #[tokio::test]
    async fn text_is_an_error_not_data() {
        let (a, b) = tokio::io::duplex(1 << 16);
        let mut raw = WebSocketStream::from_raw_socket(a, Role::Client, None).await;
        let mut io = WsIo::new(WebSocketStream::from_raw_socket(b, Role::Server, None).await);
        futures_util::SinkExt::send(&mut raw, Message::text("hello")).await.unwrap();
        assert!(io.read(&mut [0u8; 8]).await.is_err());
    }
}
