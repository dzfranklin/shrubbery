use eyre::eyre;
use futures::{SinkExt, StreamExt};
use shrubbery_common::frame::PresenceFrame;
use shrubbery_common::DocId;
use shrubbery_server::db::DocDb;
use shrubbery_server::db::UserDb;
use shrubbery_server::doc_manager::{DocHandle, DocManager};
use shrubbery_server::proto::http_multiplexer::HttpMultiplexer;
use shrubbery_server::proto::socket_processor;
use shrubbery_server::proto::socket_processor::SocketProcessor;
use shrubbery_server::state::authorizer::{self, Authorizer};
use shrubbery_server::{Frame, FrameType, FramedConnection};
use std::collections::HashMap;
use std::marker::PhantomData;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use structopt::StructOpt;
use tokio::fs::File;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::{fs, select, signal};
use tokio_native_tls as tokio_tls;
use tokio_native_tls::TlsStream;
use tokio_tls::native_tls as tls;
use tracing::{debug, error, info, trace, warn};

#[derive(Debug, StructOpt)]
struct Opts {
    #[structopt(long, short)]
    data_dir: PathBuf,

    #[structopt(long, short, default_value = "49243")]
    port: u16,

    #[structopt(long, default_value = "49244")]
    secure_port: u16,

    #[structopt(long, default_value = "80")]
    websocket_port: u16,

    #[structopt(long, default_value = "443")]
    websocket_secure_port: u16,

    #[structopt(long)]
    /// Path to a PKCS12 file containing the TLS identity to use for the server
    tls_identity: Option<PathBuf>,

    #[structopt(long)]
    /// Password for the PKCS12 file containing the TLS identity to use for the server
    tls_identity_password: Option<String>,
}

const SELF_SIGNED_IDENTITY: &[u8] = include_bytes!("../self_signed.pfx");

const CORE_WASM: &[u8] = b"TODO"; // TODO:

#[tokio::main]
async fn main() -> eyre::Result<()> {
    color_eyre::install()?;
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let opts = Opts::from_args();

    info!("Using data directory {}", opts.data_dir.display());

    fs::create_dir_all(&opts.data_dir).await?;

    let root_token_file = opts.data_dir.join("root_token");
    let root_token = match fs::read_to_string(&root_token_file).await {
        Ok(token) => {
            info!("Loaded root token from {}", root_token_file.display());
            token.trim().to_string()
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            let token = Authorizer::random_root_token();
            let mut contents = token.as_bytes().to_vec();
            contents.push(b'\n');
            fs::write(opts.data_dir.join("root_token"), contents).await?;
            warn!(
                "No root token found. A new token was generated and written to {}",
                root_token_file.display()
            );
            token
        }
        Err(err) => return Err(err.into()),
    };
    let authorizer = Authorizer::new(root_token);

    let user_db = UserDb::open(opts.data_dir.join("users"))?;
    let docs_db = DocDb::open(opts.data_dir.join("docs"))?;
    let doc_manager = DocManager::new(docs_db);

    let tls_identity = if let Some(path) = opts.tls_identity {
        let password = opts
            .tls_identity_password
            .as_ref()
            .ok_or_else(|| eyre::eyre!("--tls-identity-password must be provided"))?;
        debug!("Loading TLS identity from {}", path.display());
        let mut file = File::open(path).await?;
        let mut buf = Vec::new();
        file.read_to_end(&mut buf).await?;
        tls::Identity::from_pkcs12(&buf, password)?
    } else {
        warn!("No --tls-identity provided, using self-signed identity");
        tls::Identity::from_pkcs12(SELF_SIGNED_IDENTITY, "password")?
    };
    let tls_acceptor = tls::TlsAcceptor::new(tls_identity)?;
    let tls_acceptor = Arc::new(tokio_tls::TlsAcceptor::from(tls_acceptor));

    let listener = TcpListener::bind(("0.0.0.0", opts.port)).await?;
    info!(
        "Listening on {} for the shrubbery protocol (no TLS)",
        listener.local_addr()?
    );

    let secure_listener = TcpListener::bind(("0.0.0.0", opts.secure_port)).await?;
    info!(
        "Listening on {} for the shrubbery protocol (TLS)",
        secure_listener.local_addr()?
    );

    let websocket_listener = TcpListener::bind(("0.0.0.0", opts.websocket_port)).await?;
    info!(
        "Listening on ws://{} as a HTTP/WebSocket server",
        websocket_listener.local_addr()?
    );

    let websocket_secure_listener =
        TcpListener::bind(("0.0.0.0", opts.websocket_secure_port)).await?;
    info!(
        "Listening on on wss://{} as a HTTPS/WebSocket Secure server",
        websocket_secure_listener.local_addr()?
    );

    let shrub_processor = socket_processor::SocketProcessor::<TcpStream>::new(
        authorizer.clone(),
        doc_manager.clone(),
        user_db.clone(),
    );
    let shrubs_processor = socket_processor::SocketProcessor::<TlsStream<TcpStream>>::new(
        authorizer.clone(),
        doc_manager.clone(),
        user_db.clone(),
    );
    let http_processor = socket_processor::SocketProcessor::new(
        authorizer.clone(),
        doc_manager.clone(),
        user_db.clone(),
    );
    let tls_processor = socket_processor::SocketProcessor::new(
        authorizer.clone(),
        doc_manager.clone(),
        user_db.clone(),
    );

    let http_muxer = HttpMultiplexer::<TcpStream>::new(CORE_WASM, http_processor);
    let tls_muxer = HttpMultiplexer::<TlsStream<TcpStream>>::new(CORE_WASM, tls_processor);

    loop {
        select! {
            res = signal::ctrl_c() => {
                res.map_err(|err| eyre::eyre!("Failed to listen for ctrl-c: {}", err))?;
                info!("Received ctrl-c");
                break;
            }
            // res = listener.accept() => {
            //     let (socket, addr) = res?;
            //     let processor = shrub_processor.clone();
            //     tokio::spawn(async move {
            //         let socket = match FramedConnection::accept_shrub(socket).await {
            //             Ok(socket) => socket,
            //             Err(err) => {
            //                 info!("Connection error: {}", err);
            //                 return;
            //             }
            //         };
            //         if let Err(err) = Session::accept(shared, socket).await {
            //             info!("Connection error: {}", err);
            //         }
            //     });
            // }
            // res = secure_listener.accept() => {
            //     let (socket, addr) = res?;
            //     info!("Accepting connection from {} for the shrubbery protocol (TLS)", addr);
            //     let acceptor = tls_acceptor.clone();
            //     let shared = shared.clone();
            //     tokio::spawn(async move {
            //         let socket = match FramedConnection::accept_shrub_secure(socket, &acceptor).await {
            //             Ok(socket) => socket,
            //             Err(err) => {
            //                 info!("Connection error: {}", err);
            //                 return;
            //             }
            //         };
            //         if let Err(err) = Session::accept(shared, socket).await {
            //             info!("Connection error: {}", err);
            //         }
            //     });
            // }
            res = websocket_listener.accept() => {
                let (socket, _) = res?;
                let mux = http_muxer.clone();
                tokio::spawn(async move {
                    mux.handle(socket).await;
                });
            }
            res = websocket_secure_listener.accept() => {
                let (socket, _) = res?;
                let mux = tls_muxer.clone();
                let tls_acceptor = tls_acceptor.clone();
                tokio::spawn(async move {
                    let Ok(socket) = tls_acceptor.accept(socket).await else {
                        return;
                    };
                    mux.handle(socket).await;
                });
            }
        }
    }

    Ok(())
}
