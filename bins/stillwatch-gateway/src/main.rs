use std::{
    collections::{HashMap, VecDeque},
    fs::{File, symlink_metadata},
    io::Read,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use rustls::{
    ClientConfig, RootCertStore, ServerConfig,
    client::{verify_server_cert_signed_by_trust_anchor, verify_server_name},
    crypto::ring::default_provider,
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime},
    server::ParsedCertificate,
    version::TLS13,
};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, copy_bidirectional},
    net::{TcpListener, TcpStream},
    sync::{OwnedSemaphorePermit, Semaphore, oneshot},
    task::JoinSet,
    time::{Instant, sleep, timeout},
};
use tokio_rustls::{TlsAcceptor, TlsConnector};

const PROTOCOL: &[u8; 4] = b"SWG1";
const READY: u8 = 1;
const PING: u8 = 2;
const PONG: u8 = 3;
const OPEN: u8 = 4;
const OPENED: u8 = 5;
const HANDSHAKE: Duration = Duration::from_secs(5);
const HEARTBEAT: Duration = Duration::from_secs(10);
const CONNECTION_LIFETIME: Duration = Duration::from_secs(35 * 60);
const MAX_CONNECTORS: usize = 64;
const MAX_VISITORS: usize = 32;
const MAX_VISITORS_PER_IP: u8 = 12;
const MAX_PENDING_CONNECTORS: usize = 16;
const MAX_ACCEPTS_PER_SECOND: u8 = 80;

#[derive(Parser)]
#[command(version, about = "StillWatch outbound TLS control relay")]
struct Args {
    #[command(subcommand)]
    mode: Mode,
}

#[derive(Subcommand)]
enum Mode {
    /// Listen for public clients and authenticated outbound connectors.
    Serve {
        #[arg(long)]
        listen: SocketAddr,
        #[arg(long)]
        connector_listen: SocketAddr,
        #[arg(long)]
        certificate_der: PathBuf,
        #[arg(long)]
        private_key_der: PathBuf,
        #[arg(long)]
        token_file: PathBuf,
    },
    /// Maintain a bounded pool of outbound connections to a single gateway.
    Connect {
        #[arg(long)]
        relay: SocketAddr,
        #[arg(long)]
        relay_name: String,
        #[arg(long)]
        relay_certificate_der: PathBuf,
        #[arg(long)]
        token_file: PathBuf,
        #[arg(long)]
        upstream: SocketAddr,
        #[arg(long, default_value_t = 24, value_parser = clap::value_parser!(u8).range(2..=48))]
        connections: u8,
    },
}

#[tokio::main(worker_threads = 2)]
async fn main() -> Result<()> {
    match Args::parse().mode {
        Mode::Serve {
            listen,
            connector_listen,
            certificate_der,
            private_key_der,
            token_file,
        } => {
            let cert = CertificateDer::from(read_bounded(&certificate_der, 8192, false)?);
            let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(read_bounded(
                &private_key_der,
                8192,
                true,
            )?));
            let config = ServerConfig::builder_with_provider(Arc::new(default_provider()))
                .with_protocol_versions(&[&TLS13])?
                .with_no_client_auth()
                .with_single_cert(vec![cert], key)?;
            let secret = Sha256::digest(read_token(&token_file)?).into();
            serve(listen, connector_listen, config, secret).await
        }
        Mode::Connect {
            relay,
            relay_name,
            relay_certificate_der,
            token_file,
            upstream,
            connections,
        } => {
            ensure!(
                upstream.ip().is_loopback() && upstream.port() != 0,
                "upstream must be a loopback TLS listener"
            );
            ensure!(
                !relay.ip().is_unspecified() && !relay.ip().is_multicast() && relay.port() != 0,
                "invalid relay address"
            );
            let name = ServerName::try_from(relay_name).context("invalid relay name")?;
            let pinned = CertificateDer::from(read_bounded(&relay_certificate_der, 8192, false)?);
            let mut roots = RootCertStore::empty();
            roots
                .add(pinned.clone())
                .context("invalid relay certificate")?;
            let parsed =
                ParsedCertificate::try_from(&pinned).context("invalid relay certificate")?;
            let provider = default_provider();
            verify_server_name(&parsed, &name).context("relay certificate name mismatch")?;
            verify_server_cert_signed_by_trust_anchor(
                &parsed,
                &roots,
                &[],
                UnixTime::now(),
                provider.signature_verification_algorithms.all,
            )
            .context("relay certificate must be a current, self-signed server leaf")?;
            let config = ClientConfig::builder_with_provider(Arc::new(provider))
                .with_protocol_versions(&[&TLS13])?
                .with_root_certificates(roots)
                .with_no_client_auth();
            let dial = Arc::new(Dial {
                relay,
                name,
                pinned,
                tls: TlsConnector::from(Arc::new(config)),
                upstream,
                token: read_token(&token_file)?,
            });
            connect(dial, connections).await
        }
    }
}

fn read_bounded(path: &Path, limit: u64, private: bool) -> Result<Vec<u8>> {
    let metadata = symlink_metadata(path).context("unable to read configured relay file")?;
    ensure!(
        metadata.is_file() && metadata.len() <= limit,
        "invalid relay file"
    );
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::PermissionsExt as _;
        ensure!(
            metadata.permissions().mode() & 0o077 == 0,
            "relay credential file must be owner-only"
        );
    }
    #[cfg(not(unix))]
    let _ = private;
    let mut bytes = Vec::new();
    File::open(path)?.take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(
        !bytes.is_empty() && bytes.len() as u64 <= limit,
        "invalid relay file size"
    );
    Ok(bytes)
}

fn read_token(path: &Path) -> Result<[u8; 64]> {
    let bytes = read_bounded(path, 65, true)?;
    let [token @ .., b'\n'] = bytes.as_slice() else {
        anyhow::bail!("relay token must contain 64 hex digits followed by a newline")
    };
    ensure!(
        token.len() == 64 && token.iter().all(u8::is_ascii_hexdigit),
        "invalid relay token"
    );
    Ok(token.try_into()?)
}

type Peers = Arc<Mutex<HashMap<IpAddr, u8>>>;

struct Visitor {
    socket: TcpStream,
    prefix: [u8; 6],
    _slot: OwnedSemaphorePermit,
    _peer: PeerSlot,
}

struct PeerSlot(Peers, IpAddr);

impl PeerSlot {
    fn take(peers: &Peers, ip: IpAddr) -> Option<Self> {
        let mut active = peers.lock().expect("visitor quota lock");
        let ip = ip.to_canonical();
        let count = active.entry(ip).or_default();
        if *count == MAX_VISITORS_PER_IP {
            return None;
        }
        *count += 1;
        Some(Self(peers.clone(), ip))
    }
}

impl Drop for PeerSlot {
    fn drop(&mut self) {
        let mut active = self.0.lock().expect("visitor quota lock");
        let count = active.get_mut(&self.1).expect("an assigned visitor quota");
        *count -= 1;
        if *count == 0 {
            active.remove(&self.1);
        }
    }
}

type Lanes = Arc<Mutex<VecDeque<oneshot::Sender<Visitor>>>>;

async fn serve(
    listen: SocketAddr,
    connector_listen: SocketAddr,
    config: ServerConfig,
    digest: [u8; 32],
) -> Result<()> {
    let ingress = TcpListener::bind(listen)
        .await
        .context("bind public listener")?;
    let connectors = TcpListener::bind(connector_listen)
        .await
        .context("bind connector listener")?;
    let waiting = Lanes::default();
    let acceptor = TlsAcceptor::from(Arc::new(config));
    let handshakes = Arc::new(Semaphore::new(MAX_PENDING_CONNECTORS));
    let connector_slots = Arc::new(Semaphore::new(MAX_CONNECTORS));
    let visitor_slots = Arc::new(Semaphore::new(MAX_VISITORS));
    let peers = Peers::default();
    let mut tasks = JoinSet::new();
    let mut accepted_at = Instant::now();
    let mut accepted = 0;
    let stop = tokio::signal::ctrl_c();
    tokio::pin!(stop);
    eprintln!("StillWatch gateway listening for clients and connectors");
    loop {
        tokio::select! {
            result = &mut stop => return result.context("gateway shutdown"),
            Some(_) = tasks.join_next(), if !tasks.is_empty() => {},
            incoming = connectors.accept() => {
                let (socket, _) = incoming.context("accept connector")?;
                if let Ok(handshake) = handshakes.clone().try_acquire_owned()
                    && let Ok(slot) = connector_slots.clone().try_acquire_owned()
                {
                    let acceptor = acceptor.clone();
                    let waiting = waiting.clone();
                    tasks.spawn(async move {
                        let _slot = slot;
                        let _ = serve_lane(socket, acceptor, digest, waiting, handshake).await;
                    });
                }
            }
            incoming = ingress.accept() => {
                let (socket, remote) = incoming.context("accept client")?;
                if accepted_at.elapsed() >= Duration::from_secs(1) {
                    accepted_at = Instant::now();
                    accepted = 0;
                }
                if accepted >= MAX_ACCEPTS_PER_SECOND { continue; }
                accepted += 1;
                if let Ok(slot) = visitor_slots.clone().try_acquire_owned()
                    && let Some(peer) = PeerSlot::take(&peers, remote.ip())
                {
                    let visitor = Visitor { socket, prefix: [0; 6], _slot: slot, _peer: peer };
                    tasks.spawn(assign(visitor, waiting.clone()));
                }
            }
        }
    }
}

async fn assign(mut visitor: Visitor, waiting: Lanes) {
    if !matches!(
        timeout(HANDSHAKE, visitor.socket.read_exact(&mut visitor.prefix)).await,
        Ok(Ok(_))
    ) {
        return;
    }
    let [content, major, minor, high, low, kind] = visitor.prefix;
    let length = u16::from_be_bytes([high, low]);
    if content != 22
        || major != 3
        || !(1..=3).contains(&minor)
        || !(1..=16_384).contains(&length)
        || kind != 1
    {
        return;
    }
    let mut waiting = waiting.lock().expect("connector queue lock");
    while let Some(lane) = waiting.pop_front() {
        visitor = match lane.send(visitor) {
            Ok(()) => return,
            Err(visitor) => visitor,
        };
    }
}

async fn serve_lane(
    socket: TcpStream,
    acceptor: TlsAcceptor,
    digest: [u8; 32],
    waiting: Lanes,
    handshake: OwnedSemaphorePermit,
) -> Result<()> {
    let mut tunnel = timeout(HANDSHAKE, async {
        let mut tunnel = acceptor.accept(socket).await?;
        let mut hello = [0; 68];
        tunnel.read_exact(&mut hello).await?;
        let actual: [u8; 32] = Sha256::digest(&hello[4..]).into();
        ensure!(
            &hello[..4] == PROTOCOL && bool::from(digest.ct_eq(&actual)),
            "invalid connector authentication"
        );
        tunnel.write_u8(READY).await?;
        tunnel.flush().await?;
        Ok::<_, anyhow::Error>(tunnel)
    })
    .await??;
    drop(handshake);

    let (offer, mut claim) = oneshot::channel();
    {
        let mut waiting = waiting.lock().expect("connector queue lock");
        waiting.retain(|lane| !lane.is_closed());
        waiting.push_back(offer);
    }
    let mut visitor = loop {
        tokio::select! {
            visitor = &mut claim => break visitor?,
            _ = tunnel.read_u8() => return Ok(()),
            _ = sleep(HEARTBEAT) => {
                timeout(HANDSHAKE, async {
                    tunnel.write_u8(PING).await?;
                    tunnel.flush().await?;
                    ensure!(tunnel.read_u8().await? == PONG, "invalid connector heartbeat");
                    Ok::<_, anyhow::Error>(())
                }).await??;
            }
        }
    };
    timeout(HANDSHAKE, async {
        tunnel.write_u8(OPEN).await?;
        tunnel.flush().await?;
        ensure!(
            tunnel.read_u8().await? == OPENED,
            "connector upstream is unavailable"
        );
        tunnel.write_all(&visitor.prefix).await?;
        tunnel.flush().await?;
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    timeout(
        CONNECTION_LIFETIME,
        copy_bidirectional(&mut visitor.socket, &mut tunnel),
    )
    .await??;
    Ok(())
}

struct Dial {
    relay: SocketAddr,
    name: ServerName<'static>,
    pinned: CertificateDer<'static>,
    tls: TlsConnector,
    upstream: SocketAddr,
    token: [u8; 64],
}

async fn connect(dial: Arc<Dial>, lanes: u8) -> Result<()> {
    let mut workers = JoinSet::new();
    for index in 0..lanes {
        let dial = dial.clone();
        workers.spawn(async move {
            let stagger = Duration::from_millis(u64::from(index) * 19);
            sleep(stagger).await;
            let mut backoff = Duration::from_millis(250);
            loop {
                let started = Instant::now();
                if connector_lane(&dial).await.is_ok() || started.elapsed() >= HEARTBEAT {
                    backoff = Duration::from_millis(250);
                }
                sleep(backoff + stagger).await;
                backoff = (backoff * 2).min(Duration::from_secs(5));
            }
        });
    }
    eprintln!("StillWatch gateway connector running");
    tokio::select! {
        result = tokio::signal::ctrl_c() => result.context("connector shutdown"),
        _ = workers.join_next() => anyhow::bail!("outbound connector worker stopped"),
    }
}

async fn connector_lane(dial: &Dial) -> Result<()> {
    let mut tunnel = timeout(HANDSHAKE, async {
        let socket = TcpStream::connect(dial.relay).await?;
        let mut tunnel = dial.tls.connect(dial.name.clone(), socket).await?;
        ensure!(
            tunnel
                .get_ref()
                .1
                .peer_certificates()
                .and_then(|chain| chain.first())
                == Some(&dial.pinned),
            "relay leaf certificate changed"
        );
        tunnel.write_all(PROTOCOL).await?;
        tunnel.write_all(&dial.token).await?;
        tunnel.flush().await?;
        ensure!(
            tunnel.read_u8().await? == READY,
            "relay declined the connector"
        );
        Ok::<_, anyhow::Error>(tunnel)
    })
    .await??;
    loop {
        match timeout(HEARTBEAT + HANDSHAKE + HANDSHAKE, tunnel.read_u8()).await?? {
            PING => {
                timeout(HANDSHAKE, async {
                    tunnel.write_u8(PONG).await?;
                    tunnel.flush().await
                })
                .await??
            }
            OPEN => break,
            _ => anyhow::bail!("invalid relay command"),
        }
    }
    let mut upstream = timeout(HANDSHAKE, async {
        let upstream = TcpStream::connect(dial.upstream).await?;
        tunnel.write_u8(OPENED).await?;
        tunnel.flush().await?;
        Ok::<_, anyhow::Error>(upstream)
    })
    .await??;
    timeout(
        CONNECTION_LIFETIME,
        copy_bidirectional(&mut upstream, &mut tunnel),
    )
    .await??;
    Ok(())
}
