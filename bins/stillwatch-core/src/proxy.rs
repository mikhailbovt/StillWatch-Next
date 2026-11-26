use std::{
    fs::File,
    io::Read,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    sync::Arc,
};

use anyhow::{Context, Result, ensure};
use axum::{
    Extension, Router,
    extract::{Request, State},
    http::{StatusCode, Uri},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use rustls::{
    RootCertStore,
    client::{verify_server_cert_signed_by_trust_anchor, verify_server_name},
    pki_types::{CertificateDer, ServerName, UnixTime},
    server::ParsedCertificate,
};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use sw_api::EndpointIdentity;
use tokio::net::TcpListener;

#[derive(Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProxySettings {
    pub listen: SocketAddr,
    pub endpoint: String,
    pub certificate_der_file: PathBuf,
    pub token_file: PathBuf,
}

pub struct Proxy {
    pub identity: Arc<EndpointIdentity>,
    pub host: String,
    address: SocketAddr,
    token_digest: [u8; 32],
}

impl Proxy {
    pub fn open(settings: ProxySettings, local_digest: &[u8; 32]) -> Result<Self> {
        ensure!(
            settings.listen.ip().is_loopback() && settings.listen.port() >= 1024,
            "proxy upstream requires a dedicated loopback port above 1023"
        );
        let uri: Uri = settings
            .endpoint
            .parse()
            .context("invalid proxy endpoint")?;
        let host = uri.host().context("missing proxy DNS name")?;
        ensure!(
            uri.scheme_str() == Some("https")
                && !settings.endpoint.contains(['@', '\\', '#', '?'])
                && uri.path() == "/"
                && host.contains('.')
                && host.split('.').all(valid_dns_label)
                && host.parse::<IpAddr>().is_err()
                && uri.port_u16().is_none_or(|port| port != 0 && port != 443),
            "proxy endpoint requires a lowercase HTTPS DNS origin without a path"
        );
        let authority = uri.authority().context("missing proxy authority")?.as_str();
        let endpoint = format!("https://{authority}");
        ensure!(
            settings.endpoint == endpoint || settings.endpoint == format!("{endpoint}/"),
            "proxy endpoint must be a canonical HTTPS origin"
        );
        let server_name = ServerName::try_from(host).context("invalid proxy DNS name")?;
        let mut bytes = Vec::new();
        File::open(&settings.certificate_der_file)
            .context("open proxy endpoint certificate")?
            .take(6145)
            .read_to_end(&mut bytes)
            .context("read proxy endpoint certificate")?;
        ensure!(
            (1..=6144).contains(&bytes.len()),
            "invalid proxy endpoint certificate size"
        );
        let der = CertificateDer::from(bytes.as_slice());
        let parsed = ParsedCertificate::try_from(&der).context("invalid proxy certificate DER")?;
        verify_server_name(&parsed, &server_name)
            .context("proxy certificate must cover endpoint")?;
        let mut roots = RootCertStore::empty();
        roots
            .add(der.clone())
            .context("invalid proxy trust anchor")?;
        verify_server_cert_signed_by_trust_anchor(
            &parsed,
            &roots,
            &[],
            UnixTime::now(),
            rustls::crypto::ring::default_provider()
                .signature_verification_algorithms
                .all,
        )
        .context("proxy certificate must be a current, self-signed server leaf")?;
        let token_digest =
            crate::token::read_digest(&settings.token_file).context("proxy authentication file")?;
        ensure!(
            !bool::from(token_digest.ct_eq(local_digest)),
            "proxy authentication requires a separate token"
        );
        Ok(Self {
            identity: Arc::new(EndpointIdentity {
                endpoint,
                certificate_sha256: format!("{:x}", Sha256::digest(&bytes)),
                certificate_der: bytes,
            }),
            host: authority.to_owned(),
            address: settings.listen,
            token_digest,
        })
    }

    pub async fn bind(&self) -> Result<TcpListener> {
        TcpListener::bind(self.address)
            .await
            .context("unable to bind the proxy loopback listener")
    }

    pub fn protect(&self, router: Router) -> Router {
        router.layer(middleware::from_fn_with_state(
            self.token_digest,
            authenticate,
        ))
    }
}

fn valid_dns_label(label: &str) -> bool {
    (1..=63).contains(&label.len())
        && !label.starts_with('-')
        && !label.ends_with('-')
        && label
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

async fn authenticate(
    State(expected): State<[u8; 32]>,
    Extension(peer): Extension<SocketAddr>,
    mut request: Request,
    next: Next,
) -> Response {
    const PROXY_TOKEN: &str = "x-stillwatch-proxy-token";
    const CLIENT_ADDRESS: &str = "x-stillwatch-client-address";
    let headers = request.headers_mut();
    let authorized = {
        let mut values = headers.get_all(PROXY_TOKEN).iter();
        values.next().is_some_and(|value| {
            value.len() == 64
                && bool::from(crate::token::digest(value.as_bytes()).ct_eq(&expected))
                && values.next().is_none()
        })
    };
    if !peer.ip().is_loopback() || !authorized {
        return StatusCode::FORBIDDEN.into_response();
    }
    let mut addresses = headers.get_all(CLIENT_ADDRESS).iter();
    let client = addresses
        .next()
        .and_then(|value| value.to_str().ok()?.parse::<IpAddr>().ok());
    let Some(client) = client.filter(|_| addresses.next().is_none()) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    for name in [
        PROXY_TOKEN,
        CLIENT_ADDRESS,
        "forwarded",
        "x-forwarded-for",
        "x-forwarded-host",
        "x-forwarded-port",
        "x-forwarded-proto",
        "x-real-ip",
    ] {
        headers.remove(name);
    }
    request.extensions_mut().insert(SocketAddr::new(client, 0));
    next.run(request).await
}
