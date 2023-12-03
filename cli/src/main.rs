use colored_json::prelude::*;
use eyre::{eyre, Context};
use futures::{SinkExt, StreamExt};
use shrubbery_common::frame::{Frame, FrameType};
use shrubbery_common::framed::FramedConnection;
use structopt::StructOpt;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::TcpStream;
use tokio::{fs, select};
use tracing::{debug, error, info, trace};

#[derive(StructOpt)]
struct Opts {
    #[structopt(long, short, default_value = "49243")]
    port: u16,

    #[structopt(long, short, default_value = "localhost")]
    host: String,

    #[structopt(
        long,
        short,
        help = "Path to the file containing an authentication token. Use '-' for stdin.",
        default_value = "~/.config/shrubbery/token"
    )]
    token: String,

    #[structopt(subcommand)]
    cmd: Cmd,
}

#[derive(StructOpt)]
enum Cmd {
    Raw {
        frame: String,
        #[structopt(long)]
        skip_auth: bool,
    },
    MintToken {
        user: String,
        #[structopt(long, help = "JSON-encoded user info")]
        info: Option<String>,
        #[structopt(
            long,
            short,
            help = "Token lifetime in seconds",
            default_value = "3600"
        )]
        lifetime: u64,
    },
    RevokeTokensForUser {
        user: String,
    },
}

#[tokio::main]
async fn main() -> eyre::Result<()> {
    color_eyre::install()?;
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let opts = Opts::from_args();

    let skip_auth = matches!(
        opts.cmd,
        Cmd::Raw {
            skip_auth: true,
            ..
        }
    );

    let token = if skip_auth {
        "".to_string()
    } else if opts.token == "-" {
        BufReader::new(tokio::io::stdin())
            .lines()
            .next_line()
            .await?
            .ok_or(eyre!("expected a token on stdin"))?
    } else {
        let path = expanduser::expanduser(opts.token.as_str())?;
        let contents = fs::read_to_string(path).await?;
        contents.trim().to_string()
    };

    let socket = TcpStream::connect((opts.host.as_str(), opts.port)).await?;
    let mut socket = FramedConnection::establish_shrub(socket).await?;

    if !skip_auth {
        authenticate(&mut socket, token).await?;
    }

    info!("Connected to {}:{}", opts.host, opts.port);

    match opts.cmd {
        Cmd::MintToken {
            user,
            info,
            lifetime,
        } => {
            let info = info.map(|s| serde_json::from_str(s.as_str())).transpose()?;
            let frame = Frame::new(
                -2,
                FrameType::MintToken {
                    user,
                    info,
                    lifetime_seconds: lifetime,
                },
            );
            socket.send(frame).await?;
            let reply = read_reply(&mut socket, -2).await?;
            match reply.frame {
                FrameType::MintTokenResponse { token } => {
                    println!("{}", token);
                    Ok(())
                }
                FrameType::Error { error } => Err(eyre!("error minting token: {}", error)),
                _ => Err(eyre!("unexpected reply frame: {:?}", reply)),
            }
        }
        Cmd::RevokeTokensForUser { user } => {
            let frame = Frame::new(-2, FrameType::RevokeTokensForUser { user });
            socket.send(frame).await?;
            let reply = read_reply(&mut socket, -2).await?;
            match reply.frame {
                FrameType::Ok => {
                    println!("ok");
                    Ok(())
                }
                FrameType::Error { error } => Err(eyre!("error revoking tokens: {}", error)),
                _ => Err(eyre!("unexpected reply frame: {:?}", reply)),
            }
        }
        Cmd::Raw {
            frame,
            skip_auth: _,
        } => {
            let frame = serde_json::from_str(&frame).wrap_err("input is not a valid frame")?;
            socket.send(frame).await?;
            trace!("sent frame");

            let mut reader = BufReader::new(tokio::io::stdin()).lines();
            loop {
                select! {
                    frame = socket.next() => {
                        let Some(frame) = frame else {
                            info!("Connection closed");
                            return Ok(());
                        };
                        let frame = frame?;
                        let json = serde_json::to_string_pretty(&frame)?;
                        print!("\n{}\n\n", json.to_colored_json_auto()?);
                    }

                    line = reader.next_line() => {
                        let Some(line) = line? else {
                            return Ok(())
                        };
                        let frame = match serde_json::from_str(&line) {
                            Ok(frame) => frame,
                            Err(err) => {
                                error!("invalid frame: {}", err);
                                continue;
                            },
                        };
                        socket.send(frame).await?;
                        trace!("sent frame");
                    }
                }
            }
        }
    }
}

async fn authenticate(socket: &mut FramedConnection, token: String) -> eyre::Result<()> {
    socket
        .send(Frame::new(-1, FrameType::Authenticate { token }))
        .await?;
    let reply = read_reply(socket, -1).await?;
    match reply.frame {
        FrameType::Ok => return Ok(()),
        FrameType::Error { error: msg } => return Err(eyre!("error authenticating: {}", msg)),
        _ => return Err(eyre!("unexpected reply frame: {:?}", reply)),
    }
}

async fn read_reply(socket: &mut FramedConnection, id: i32) -> eyre::Result<Frame> {
    loop {
        let Some(frame) = socket.next().await else {
            return Err(eyre!("unexpected close"));
        };
        let frame = frame?;
        if frame.reply_to != Some(id) {
            debug!("ignoring {:?} (expected reply to {}", frame, id);
            continue;
        }
        return Ok(frame);
    }
}
