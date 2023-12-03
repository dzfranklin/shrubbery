use crate::codec::ShrubCodec;
use crate::frame::Frame;
use futures::{Sink, Stream, StreamExt};
use pin_project::pin_project;
use std::fmt::Formatter;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_native_tls::{TlsAcceptor, TlsStream};
use tokio_tungstenite::{tungstenite, WebSocketStream};
use tokio_util::codec::{Decoder, Framed};

#[pin_project(project = FramedConnectionProj)]
pub enum FramedConnection {
    Shrub(#[pin] Framed<TcpStream, ShrubCodec>),
    ShrubSecure(#[pin] Framed<TlsStream<TcpStream>, ShrubCodec>),
    WebSocket(#[pin] WebSocketStream<TcpStream>),
    WebSocketSecure(#[pin] WebSocketStream<TlsStream<TcpStream>>),
}

impl FramedConnection {
    pub async fn accept_shrub(mut socket: TcpStream) -> eyre::Result<Self> {
        read_shrub_version_header(&mut socket).await?;
        Ok(Self::Shrub(ShrubCodec::new().framed(socket)))
    }

    pub async fn establish_shrub(mut socket: TcpStream) -> eyre::Result<Self> {
        send_shrub_version_header(&mut socket).await?;
        Ok(Self::Shrub(ShrubCodec::new().framed(socket)))
    }

    pub async fn accept_shrub_secure(
        socket: TcpStream,
        acceptor: &TlsAcceptor,
    ) -> eyre::Result<Self> {
        let mut socket = acceptor.accept(socket).await?;
        read_shrub_version_header(&mut socket).await?;
        Ok(Self::ShrubSecure(ShrubCodec::new().framed(socket)))
    }

    pub async fn accept_websocket(socket: TcpStream) -> eyre::Result<Self> {
        let mut socket = tokio_tungstenite::accept_async(socket).await?;
        read_websocket_version_message(&mut socket).await?;
        Ok(Self::WebSocket(socket))
    }

    pub async fn accept_websocket_secure(
        socket: TcpStream,
        acceptor: &TlsAcceptor,
    ) -> eyre::Result<Self> {
        let socket = acceptor.accept(socket).await?;
        let mut socket = tokio_tungstenite::accept_async(socket).await?;
        read_websocket_version_message(&mut socket).await?;
        Ok(Self::WebSocketSecure(socket))
    }
}

impl Stream for FramedConnection {
    type Item = Result<Frame, std::io::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.project() {
            FramedConnectionProj::Shrub(inner) => inner.poll_next(cx),
            FramedConnectionProj::ShrubSecure(inner) => inner.poll_next(cx),
            FramedConnectionProj::WebSocket(inner) => poll_websocket_next(inner, cx),
            FramedConnectionProj::WebSocketSecure(inner) => poll_websocket_next(inner, cx),
        }
    }
}

fn poll_websocket_next<S>(
    inner: Pin<&mut WebSocketStream<S>>,
    cx: &mut Context<'_>,
) -> Poll<Option<Result<Frame, std::io::Error>>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let res = match inner.poll_next(cx) {
        Poll::Pending => return Poll::Pending,
        Poll::Ready(None) => return Poll::Ready(None),
        Poll::Ready(Some(res)) => res,
    };
    let msg = match map_tungstenite_result(res) {
        Ok(msg) => msg,
        Err(err) => return Poll::Ready(Some(Err(err))),
    };
    let msg = match msg {
        tungstenite::Message::Text(text) => text,
        tungstenite::Message::Binary(_) => {
            return Poll::Ready(Some(Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "unexpected binary message",
            ))))
        }
        _ => return Poll::Pending,
    };
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

impl Sink<Frame> for FramedConnection {
    type Error = std::io::Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.project() {
            FramedConnectionProj::Shrub(inner) => inner.poll_ready(cx),
            FramedConnectionProj::ShrubSecure(inner) => inner.poll_ready(cx),
            FramedConnectionProj::WebSocket(inner) => {
                map_tungstenite_poll_result(inner.poll_ready(cx))
            }
            FramedConnectionProj::WebSocketSecure(inner) => {
                map_tungstenite_poll_result(inner.poll_ready(cx))
            }
        }
    }

    fn start_send(self: Pin<&mut Self>, item: Frame) -> Result<(), Self::Error> {
        match self.project() {
            FramedConnectionProj::Shrub(inner) => inner.start_send(item),
            FramedConnectionProj::ShrubSecure(inner) => inner.start_send(item),
            FramedConnectionProj::WebSocket(inner) => map_tungstenite_result(
                inner.start_send(tungstenite::Message::Text(serde_json::to_string(&item)?)),
            ),
            FramedConnectionProj::WebSocketSecure(inner) => map_tungstenite_result(
                inner.start_send(tungstenite::Message::Text(serde_json::to_string(&item)?)),
            ),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.project() {
            FramedConnectionProj::Shrub(inner) => inner.poll_flush(cx),
            FramedConnectionProj::ShrubSecure(inner) => inner.poll_flush(cx),
            FramedConnectionProj::WebSocket(inner) => {
                map_tungstenite_poll_result(inner.poll_flush(cx))
            }
            FramedConnectionProj::WebSocketSecure(inner) => {
                map_tungstenite_poll_result(inner.poll_flush(cx))
            }
        }
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.project() {
            FramedConnectionProj::Shrub(inner) => inner.poll_close(cx),
            FramedConnectionProj::ShrubSecure(inner) => inner.poll_close(cx),
            FramedConnectionProj::WebSocket(inner) => {
                map_tungstenite_poll_result(inner.poll_close(cx))
            }
            FramedConnectionProj::WebSocketSecure(inner) => {
                map_tungstenite_poll_result(inner.poll_close(cx))
            }
        }
    }
}

fn map_tungstenite_poll_result<T>(
    res: Poll<Result<T, tungstenite::Error>>,
) -> Poll<Result<T, std::io::Error>> {
    match res {
        Poll::Pending => Poll::Pending,
        Poll::Ready(res) => Poll::Ready(map_tungstenite_result(res)),
    }
}

fn map_tungstenite_result<T>(res: Result<T, tungstenite::Error>) -> Result<T, std::io::Error> {
    match res {
        Ok(val) => Ok(val),
        Err(tungstenite::Error::Io(err)) => Err(err),
        Err(err) => Err(std::io::Error::new(std::io::ErrorKind::Other, err)),
    }
}

async fn read_shrub_version_header<T>(mut socket: T) -> eyre::Result<()>
where
    T: AsyncRead + Unpin,
{
    let mut header = [0; b"shrub_\n".len()];
    socket.read_exact(&mut header).await?;
    if header != *b"shrub1\n" {
        return Err(eyre::eyre!("expected shrub header"));
    }
    Ok(())
}

async fn send_shrub_version_header<T>(mut socket: T) -> std::io::Result<()>
where
    T: AsyncWrite + Unpin,
{
    socket.write_all(b"shrub1\n").await
}

async fn read_websocket_version_message<T>(mut socket: T) -> eyre::Result<()>
where
    T: Unpin + Stream<Item = Result<tungstenite::Message, tungstenite::Error>>,
{
    let msg = loop {
        let Some(msg) = socket.next().await.transpose()? else {
            return Err(eyre::eyre!("closed"));
        };
        match msg {
            tungstenite::Message::Text(text) => break text,
            tungstenite::Message::Binary(_) => {
                return Err(eyre::eyre!("unexpected binary message"));
            }
            _ => continue,
        };
    };

    if msg != "shrub1" {
        return Err(eyre::eyre!("expected shrub version message"));
    }
    Ok(())
}

impl std::fmt::Debug for FramedConnection {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FramedConnection").finish_non_exhaustive()
    }
}
