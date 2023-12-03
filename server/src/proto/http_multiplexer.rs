use crate::proto::framed_websocket;
use crate::proto::socket_processor;
use bytes::BytesMut;
use http::response;
use std::fmt::Write;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tracing::{debug, trace};

pub struct HttpMultiplexer<Socket> {
    processor: socket_processor::SocketProcessor<framed_websocket::Adapter<Socket>>,
    core_wasm: &'static [u8],
}

impl<S> Clone for HttpMultiplexer<S> {
    fn clone(&self) -> Self {
        Self {
            processor: self.processor.clone(),
            core_wasm: self.core_wasm,
        }
    }
}

impl<Socket> HttpMultiplexer<Socket>
where
    Socket: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    pub fn new(
        core_wasm: &'static [u8],
        processor: socket_processor::SocketProcessor<framed_websocket::Adapter<Socket>>,
    ) -> Self {
        Self {
            processor,
            core_wasm,
        }
    }

    pub async fn handle(&self, mut socket: Socket) {
        let mut buf = BytesMut::new();
        loop {
            let mut headers = [httparse::EMPTY_HEADER; 64];
            if socket.read_buf(&mut buf).await.is_err() {
                debug!("rejecting: failed to read");
                return;
            }
            let mut req = httparse::Request::new(&mut headers);
            match req.parse(&buf) {
                Ok(httparse::Status::Complete(_)) => {}
                Ok(httparse::Status::Partial) => {
                    continue;
                }
                Err(e) => {
                    debug!("rejecting: failed to parse: {:?}", e);
                    return;
                }
            }
            trace!("parsed: {:?}", req);
            let Some(method) = req.method else {
                debug!("rejecting: missing method");
                return;
            };
            let Some(path) = req.path else {
                debug!("rejecting: missing path");
                return;
            };

            match (method, path) {
                ("GET", "/core.wasm") => {
                    self.respond_core_wasm(socket).await;
                    return;
                }
                ("GET", "/socket") => {
                    let mut version = None;
                    let mut key = None;
                    for header in &headers {
                        match header.name {
                            "Sec-WebSocket-Version" => version = Some(header.value),
                            "Sec-WebSocket-Key" => key = Some(header.value),
                            _ => {}
                        }
                    }
                    if version != Some(b"13") {
                        debug!("rejecting: unsupported websocket version: {:?}", version);
                        return;
                    }
                    let Some(key) = key else {
                        debug!("rejecting: missing websocket key");
                        return;
                    };
                    self.accept_websocket(socket, key).await;
                    return;
                }
                _ => {
                    debug!("not found: {} {}", method, path);
                    Self::respond_404(socket).await;
                    return;
                }
            }
        }
    }

    async fn respond_core_wasm(&self, mut socket: Socket) {
        let response = response::Builder::new()
            .status(200)
            .header("Content-Type", "application/wasm")
            .header("Content-Length", self.core_wasm.len())
            .body(())
            .unwrap();
        let buf = http_head(response);
        let _ = socket.write_all(&buf).await;
        let _ = socket.write_all(&self.core_wasm).await;
    }

    async fn accept_websocket(&self, mut socket: Socket, key: &[u8]) {
        let response = response::Builder::new()
            .status(101)
            .header("Upgrade", "websocket")
            .header("Connection", "Upgrade")
            .header("Sec-WebSocket-Accept", derive_accept_key(key))
            .body(())
            .unwrap();
        let buf = http_head(response);
        let _ = socket.write_all(&buf).await;

        let socket = tokio_tungstenite::WebSocketStream::from_raw_socket(
            socket,
            tokio_tungstenite::tungstenite::protocol::Role::Server,
            None,
        )
        .await;

        self.processor
            .accept(framed_websocket::Adapter::new(socket))
            .await;
    }

    async fn respond_404(mut socket: Socket) {
        let body = b"Not Found";
        let response = response::Builder::new()
            .status(404)
            .header("Content-Type", "text/plain")
            .header("Content-Length", body.len())
            .body(())
            .unwrap();
        let mut buf = http_head(response);
        buf.extend_from_slice(body);
        let _ = socket.write_all(&buf).await;
    }
}

fn http_head(response: http::Response<()>) -> BytesMut {
    let mut buf = BytesMut::new();
    let _ = buf.write_str("HTTP/1.1 ");
    let _ = buf.write_str(response.status().as_str());
    if let Some(reason) = response.status().canonical_reason() {
        let _ = buf.write_str(" ");
        let _ = buf.write_str(reason);
    }
    let _ = buf.write_str("\r\n");
    for (name, value) in response.headers() {
        let _ = buf.write_str(name.as_str());
        let _ = buf.write_str(": ");
        let _ = buf.write_str(value.to_str().unwrap());
        let _ = buf.write_str("\r\n");
    }
    let _ = buf.write_str("\r\n");
    buf
}

fn derive_accept_key(key: &[u8]) -> String {
    use base64::prelude::*;
    use sha1::{Digest, Sha1};
    let mut hasher = Sha1::new();
    hasher.update(key);
    hasher.update(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
    BASE64_STANDARD.encode(hasher.finalize())
}
