use crate::Frame;
use pin_project::pin_project;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_tungstenite::{
    tungstenite::{Error, Message},
    WebSocketStream,
};
use tracing::debug;

#[pin_project]
pub struct Adapter<S> {
    #[pin]
    inner: WebSocketStream<S>,
    shrub_handshake_state: ShrubHandshakeState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShrubHandshakeState {
    Pre,
    Post,
    Missing,
}

impl<S> Adapter<S> {
    pub(super) fn new(inner: WebSocketStream<S>) -> Self {
        Self {
            inner,
            shrub_handshake_state: ShrubHandshakeState::Pre,
        }
    }
}

impl<S> futures::Stream for Adapter<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    type Item = std::io::Result<Frame>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.project();
        let res = match this.inner.poll_next(cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(res) => res,
        };
        let res = match res {
            Some(res) => res,
            None => return Poll::Pending,
        };
        let msg = match res {
            Ok(msg) => msg,
            Err(Error::ConnectionClosed) => return Poll::Ready(None),
            Err(err) => return Poll::Ready(Some(Err(transpose_to_io_error(err)))),
        };
        let msg = match msg {
            Message::Text(text) => text,
            Message::Binary(_) => {
                // ignore binary frames for forwards compatibility
                debug!("ignoring binary websocket message");
                return Poll::Pending;
            }
            _ => return Poll::Pending, // ignore control frames
        };

        match this.shrub_handshake_state {
            ShrubHandshakeState::Pre => {
                if msg != "shrub1\n" {
                    return Poll::Ready(Some(Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "expected 'shrub' handshake",
                    ))));
                }
                *this.shrub_handshake_state = ShrubHandshakeState::Post;
                return Poll::Pending;
            }
            ShrubHandshakeState::Missing => {
                return Poll::Ready(Some(Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "missing 'shrub' handshake",
                ))));
            }
            ShrubHandshakeState::Post => {
                let frame = match serde_json::from_str(&msg) {
                    Ok(frame) => frame,
                    Err(err) => {
                        return Poll::Ready(Some(Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            err,
                        ))))
                    }
                };
                Poll::Ready(Some(Ok(frame)))
            }
        }
    }
}

impl<S> futures::Sink<Frame> for Adapter<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    type Error = std::io::Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project()
            .inner
            .poll_ready(cx)
            .map_err(transpose_to_io_error)
    }

    fn start_send(self: Pin<&mut Self>, item: Frame) -> Result<(), Self::Error> {
        let msg = match serde_json::to_string(&item) {
            Ok(msg) => msg,
            Err(err) => return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, err)),
        };
        self.project()
            .inner
            .start_send(Message::Text(msg))
            .map_err(transpose_to_io_error)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project()
            .inner
            .poll_flush(cx)
            .map_err(transpose_to_io_error)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project()
            .inner
            .poll_close(cx)
            .map_err(transpose_to_io_error)
    }
}

fn transpose_to_io_error(err: Error) -> std::io::Error {
    match err {
        Error::Io(err) => err,
        err => std::io::Error::new(std::io::ErrorKind::Other, err),
    }
}
