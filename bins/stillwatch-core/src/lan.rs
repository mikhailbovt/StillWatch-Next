use std::{
    fs::{self, File},
    io::{Read, Write},
    net::{IpAddr, SocketAddr},
    path::Path,
    sync::Arc,
};

use anyhow::{Context, Result, anyhow, ensure};
use axum::Router;
use rcgen::{CertificateParams, KeyPair};
use rustls::{
    ServerConfig,
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
};
use sha2::{Digest, Sha256};
use sw_api::EndpointIdentity;
use tokio::{net::TcpListener, sync::watch};
use tokio_rustls::TlsAcceptor;

pub struct Lan {
    pub identity: Arc<EndpointIdentity>,
    pub host: String,
    tls: TlsAcceptor,
    address: SocketAddr,
    advertise: bool,
}

impl Lan {
    pub fn open(root: &Path, address: SocketAddr, advertise: bool) -> Result<Self> {
        let ip = address.ip();
        ensure!(
            address.port() >= 1024
                && !ip.is_unspecified()
                && !ip.is_multicast()
                && matches!(ip, IpAddr::V4(v) if v.is_private() || v.is_loopback()
                || (v.octets()[0] == 100 && (64..=127).contains(&v.octets()[1])))
                || (address.port() >= 1024
                    && matches!(ip, IpAddr::V6(v) if v.is_unique_local() || v.is_loopback())),
            "LAN listener must be a specific private, CGNAT, ULA or loopback address"
        );
        ensure!(
            !matches!(address, SocketAddr::V6(v) if v.scope_id() != 0),
            "LAN identity requires an address without an interface suffix"
        );
        let path = root.join("lan-identity");
        let (cert, key) = load_or_generate(&path, ip)?;
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let tls = ServerConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(cert.clone())],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key)),
            )?;
        let host = address.to_string();
        Ok(Self {
            identity: Arc::new(EndpointIdentity {
                endpoint: format!("https://{host}"),
                certificate_sha256: format!("{:x}", Sha256::digest(&cert)),
                certificate_der: cert,
            }),
            host,
            tls: TlsAcceptor::from(Arc::new(tls)),
            address,
            advertise,
        })
    }

    pub async fn bind(&self) -> Result<TcpListener> {
        TcpListener::bind(self.address)
            .await
            .context("unable to bind private TLS listener")
    }

    pub fn bind_ip(&self) -> IpAddr {
        self.address.ip()
    }

    pub async fn serve(
        self,
        listener: TcpListener,
        router: Router,
        instance: String,
        shutdown: watch::Receiver<bool>,
    ) {
        let discovery = if self.advertise && !self.address.ip().is_loopback() {
            self.advertise(&instance)
                .map_err(|_| eprintln!("StillWatch mDNS registration failed"))
                .ok()
        } else {
            None
        };
        crate::network::serve(listener, router, Some(self.tls), shutdown).await;
        if let Some((daemon, name)) = discovery {
            let _ = daemon.unregister(&name);
            let _ = daemon.shutdown();
        }
    }

    fn advertise(&self, instance: &str) -> Result<(mdns_sd::ServiceDaemon, String)> {
        let daemon = mdns_sd::ServiceDaemon::new()?;
        let name = format!("StillWatch {}", &instance[..8]);
        let hostname = format!("stillwatch-{instance}.local.");
        let service = mdns_sd::ServiceInfo::new(
            "_stillwatch._tcp.local.",
            &name,
            &hostname,
            self.address.ip().to_string(),
            self.address.port(),
            &[("instance_id", instance), ("api", "2.0")][..],
        )?;
        let full = service.get_fullname().to_owned();
        daemon.register(service)?;
        Ok((daemon, full))
    }
}

fn load_or_generate(path: &Path, ip: IpAddr) -> Result<(Vec<u8>, Vec<u8>)> {
    if !path.exists() {
        let start = time::OffsetDateTime::now_utc() - time::Duration::minutes(5);
        let mut params = CertificateParams::new(vec![ip.to_string()])?;
        params.not_before = start;
        params.not_after = start + time::Duration::days(365 * 2);
        params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
        let key = KeyPair::generate()?;
        let cert = params.self_signed(&key)?;
        let mut stage =
            tempfile::NamedTempFile::new_in(path.parent().context("LAN identity directory")?)?;
        let bound = ip.to_string();
        writeln!(stage, "{bound}")?;
        let cert = cert.der().as_ref();
        stage.write_all(&(cert.len() as u32).to_le_bytes())?;
        stage.write_all(cert)?;
        stage.write_all(&key.serialize_der())?;
        stage.as_file().sync_all()?;
        stage.persist_noclobber(path).map_err(|error| error.error)?;
    }
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.file_type().is_file() && metadata.len() <= 8 * 1024,
        "invalid LAN identity file"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            metadata.permissions().mode() & 0o077 == 0,
            "LAN identity file must be private"
        );
    }
    let mut bytes = Vec::new();
    File::open(path)?
        .take(8 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    let separator = bytes
        .iter()
        .position(|&b| b == b'\n')
        .ok_or_else(|| anyhow!("invalid LAN identity file"))?;
    let (address, rest) = (&bytes[..separator], &bytes[separator + 1..]);
    ensure!(
        std::str::from_utf8(address)?.parse::<IpAddr>()? == ip,
        "LAN IP changed. Preserve the old identity and issue a new pairing offer for the new endpoint"
    );
    let (len, keys) = rest
        .split_first_chunk::<4>()
        .ok_or_else(|| anyhow!("invalid LAN identity file"))?;
    let cert_len = u32::from_le_bytes(*len) as usize;
    ensure!(
        (1..keys.len()).contains(&cert_len),
        "invalid LAN identity file"
    );
    let (cert, key) = keys.split_at(cert_len);
    Ok((cert.to_vec(), key.to_vec()))
}
