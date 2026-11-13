use std::{
    collections::{BTreeMap, HashSet},
    net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket as StdUdpSocket},
    time::Duration,
};

use base64::{Engine, engine::general_purpose::STANDARD};
use digest_auth::{AlgorithmType, AuthContext};
use reqwest::{Certificate, Client, StatusCode, Url, header, redirect};
use roxmltree::{Document, Node, ParsingOptions};
use socket2::SockRef;
use sw_protocol::{
    CameraSource, OnvifDevice, OnvifMedia, OnvifProfile, RtspTransport, worker::CameraCredentials,
};
use thiserror::Error;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::{net::UdpSocket, sync::Semaphore, time::Instant};
use uuid::Uuid;

const SOAP: &str = "http://www.w3.org/2003/05/soap-envelope";
const DEVICE: &str = "http://www.onvif.org/ver10/device/wsdl";
const MEDIA1: &str = "http://www.onvif.org/ver10/media/wsdl";
const MEDIA2: &str = "http://www.onvif.org/ver20/media/wsdl";
const TYPES: &str = "http://www.onvif.org/ver10/schema";
const DISCOVERY: &str = "http://schemas.xmlsoap.org/ws/2005/04/discovery";
const DISCOVERY_ADDRESSING: &str = "http://schemas.xmlsoap.org/ws/2004/08/addressing";
const DISCOVERY_GROUP: (Ipv4Addr, u16) = (Ipv4Addr::new(239, 255, 255, 250), 3702);
const MAX_DISCOVERY_BYTES: usize = 8192;
const MAX_SOAP_BYTES: usize = 128 * 1024;
const MAX_RESULTS: usize = 64;

pub struct OnvifConfig {
    pub interface: Option<Ipv4Addr>,
    pub device_endpoints: Vec<String>,
    pub trusted_ca: Option<std::path::PathBuf>,
}

#[derive(Debug, Error)]
pub enum OnvifError {
    #[error("invalid ONVIF settings")]
    Configuration,
    #[error("ONVIF multicast is not configured on this Core")]
    DiscoveryDisabled,
    #[error("ONVIF request is invalid or its device endpoint is outside the configured scope")]
    Invalid,
    #[error("ONVIF resources are busy")]
    Busy,
    #[error("ONVIF camera authentication failed")]
    Authentication,
    #[error("ONVIF device or discovery interface is unavailable")]
    Unavailable,
    #[error("ONVIF device returned an invalid or unsupported response")]
    Response,
}

pub(crate) struct Onvif {
    client: Client,
    interface: Option<Ipv4Addr>,
    endpoints: HashSet<Url>,
    scan: Semaphore,
    soap: Semaphore,
}

impl Onvif {
    pub(crate) fn new(config: OnvifConfig) -> Result<Self, OnvifError> {
        if config.interface.is_some_and(|interface| {
            interface.is_unspecified() || interface.is_multicast() || interface.is_broadcast()
        }) || config.device_endpoints.len() > 256
        {
            return Err(OnvifError::Configuration);
        }
        let endpoints: HashSet<Url> = config
            .device_endpoints
            .iter()
            .map(|value| endpoint(value).ok_or(OnvifError::Configuration))
            .collect::<Result<_, _>>()?;
        let roots = if let Some(path) = config.trusted_ca {
            use std::io::Read;
            let mut bytes = Vec::new();
            std::fs::File::open(path)
                .and_then(|file| file.take(256 * 1024 + 1).read_to_end(&mut bytes))
                .map_err(|_| OnvifError::Configuration)?;
            if bytes.len() > 256 * 1024 {
                return Err(OnvifError::Configuration);
            }
            let certs =
                Certificate::from_pem_bundle(&bytes).map_err(|_| OnvifError::Configuration)?;
            if certs.is_empty() || certs.len() > 32 {
                return Err(OnvifError::Configuration);
            }
            certs
        } else {
            Vec::new()
        };
        if roots.is_empty()
            && endpoints
                .iter()
                .any(|endpoint| endpoint.scheme() == "https")
        {
            return Err(OnvifError::Configuration);
        }
        let client = Client::builder()
            .no_proxy()
            .tls_certs_only(roots)
            .redirect(redirect::Policy::none())
            .retry(reqwest::retry::never())
            .http1_only()
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(3))
            .pool_max_idle_per_host(1)
            .pool_idle_timeout(Duration::from_secs(10))
            .build()
            .map_err(|_| OnvifError::Configuration)?;
        Ok(Self {
            client,
            interface: config.interface,
            endpoints,
            scan: Semaphore::new(1),
            soap: Semaphore::new(2),
        })
    }

    pub(crate) fn allowed_device(&self, value: &str) -> Result<Url, OnvifError> {
        endpoint(value)
            .filter(|candidate| self.endpoints.contains(candidate))
            .ok_or(OnvifError::Invalid)
    }

    pub(crate) async fn discover(&self) -> Result<Vec<OnvifDevice>, OnvifError> {
        let interface = self.interface.ok_or(OnvifError::DiscoveryDisabled)?;
        let _slot = self.scan.try_acquire().map_err(|_| OnvifError::Busy)?;
        let socket = StdUdpSocket::bind((interface, 0)).map_err(|_| OnvifError::Unavailable)?;
        let multicast = SockRef::from(&socket);
        multicast
            .set_multicast_if_v4(&interface)
            .and_then(|()| multicast.set_multicast_ttl_v4(1))
            .and_then(|()| socket.set_nonblocking(true))
            .map_err(|_| OnvifError::Unavailable)?;
        let socket = UdpSocket::from_std(socket).map_err(|_| OnvifError::Unavailable)?;
        let id = format!("urn:uuid:{}", Uuid::new_v4());
        let probe = format!(
            r#"<s:Envelope xmlns:s="{SOAP}" xmlns:a="{DISCOVERY_ADDRESSING}" xmlns:d="{DISCOVERY}" xmlns:n="http://www.onvif.org/ver10/network/wsdl"><s:Header><a:MessageID>{id}</a:MessageID><a:To>urn:schemas-xmlsoap-org:ws:2005:04:discovery</a:To><a:Action>{DISCOVERY}/Probe</a:Action></s:Header><s:Body><d:Probe><d:Types>n:NetworkVideoTransmitter</d:Types></d:Probe></s:Body></s:Envelope>"#
        );
        socket
            .send_to(probe.as_bytes(), DISCOVERY_GROUP)
            .await
            .map_err(|_| OnvifError::Unavailable)?;
        let stop = Instant::now() + Duration::from_secs(2);
        let repeat = tokio::time::sleep(Duration::from_millis(300));
        tokio::pin!(repeat);
        let mut repeated = false;
        let mut replies = 0;
        let mut devices = BTreeMap::new();
        let mut buffer = [0; MAX_DISCOVERY_BYTES + 1];
        while replies < 128 && devices.len() < MAX_RESULTS {
            tokio::select! {
                biased;
                () = tokio::time::sleep_until(stop) => break,
                () = &mut repeat, if !repeated => {
                    repeated = true;
                    let _ = socket.try_send_to(probe.as_bytes(), DISCOVERY_GROUP.into());
                }
                received = socket.recv_from(&mut buffer) => {
                    replies += 1;
                    // Winsock reports a truncated datagram as an error. Discard it too.
                    if let Ok((size, sender)) = received
                        && size <= MAX_DISCOVERY_BYTES
                    {
                        for device in probe_matches(&buffer[..size], &id, sender.ip(), &self.endpoints) {
                            devices.insert(device.endpoint.clone(), device);
                            if devices.len() == MAX_RESULTS {
                                break;
                            }
                        }
                    }
                }
            }
        }
        Ok(devices.into_values().collect())
    }

    pub(crate) async fn profiles(
        &self,
        device: &Url,
        credentials: Option<&CameraCredentials>,
    ) -> Result<Vec<OnvifProfile>, OnvifError> {
        let _slot = self.soap.try_acquire().map_err(|_| OnvifError::Busy)?;
        let services = self.services(device, credentials).await?;
        let (media2, media1) = tokio::join!(
            self.read_profiles(services.0.as_ref(), OnvifMedia::Media2, credentials),
            self.read_profiles(services.1.as_ref(), OnvifMedia::Media1, credentials)
        );
        match (media2, media1) {
            (Ok(mut modern), Ok(older)) => {
                modern.extend(older.into_iter().take(MAX_RESULTS - modern.len()));
                Ok(modern)
            }
            (Ok(valid), Err(_)) | (Err(_), Ok(valid)) if !valid.is_empty() => Ok(valid),
            (Err(error), _) | (_, Err(error)) => Err(error),
        }
    }

    async fn read_profiles(
        &self,
        endpoint: Option<&Url>,
        media: OnvifMedia,
        credentials: Option<&CameraCredentials>,
    ) -> Result<Vec<OnvifProfile>, OnvifError> {
        let Some(endpoint) = endpoint else {
            return Ok(Vec::new());
        };
        let namespace = media_namespace(media);
        let request = match media {
            OnvifMedia::Media1 => "<m:GetProfiles/>",
            OnvifMedia::Media2 => "<m:GetProfiles><m:Type>VideoEncoder</m:Type></m:GetProfiles>",
        };
        let response = self
            .soap(endpoint, credentials, namespace, "GetProfiles", request)
            .await?;
        let doc = parse_xml(&response).ok_or(OnvifError::Response)?;
        let response = soap_result(&doc, namespace, "GetProfilesResponse")?;
        let mut tokens = HashSet::new();
        Ok(response
            .children()
            .filter(|node| node.has_tag_name((namespace, "Profiles")))
            .filter_map(|node| {
                let token = node.attribute("token").filter(|value| valid_token(value))?;
                let encoder = match media {
                    OnvifMedia::Media1 => child(node, TYPES, "VideoEncoderConfiguration"),
                    OnvifMedia::Media2 => child(node, MEDIA2, "Configurations")
                        .and_then(|configuration| child(configuration, MEDIA2, "VideoEncoder")),
                };
                if encoder.is_none() || !tokens.insert(token) {
                    return None;
                }
                let name = child(node, namespace, "Name")
                    .or_else(|| child(node, TYPES, "Name"))
                    .and_then(|node| node.text())
                    .map(str::trim)
                    .filter(|value| !value.is_empty() && !value.chars().any(char::is_control))
                    .unwrap_or(token)
                    .chars()
                    .take(80)
                    .collect();
                Some(OnvifProfile {
                    media,
                    token: token.into(),
                    name,
                })
            })
            .take(MAX_RESULTS)
            .collect())
    }

    pub(crate) async fn stream(
        &self,
        device: &Url,
        credentials: Option<&CameraCredentials>,
        credential_ref: Option<Uuid>,
        media: OnvifMedia,
        token: &str,
    ) -> Result<CameraSource, OnvifError> {
        if !valid_token(token) {
            return Err(OnvifError::Invalid);
        }
        let _slot = self.soap.try_acquire().map_err(|_| OnvifError::Busy)?;
        let services = self.services(device, credentials).await?;
        let (endpoint, setup) = match media {
            OnvifMedia::Media1 => (
                services.1,
                "<m:StreamSetup><tt:Stream>RTP-Unicast</tt:Stream><tt:Transport><tt:Protocol>RTSP</tt:Protocol></tt:Transport></m:StreamSetup>",
            ),
            OnvifMedia::Media2 => (services.0, "<m:Protocol>RTSP</m:Protocol>"),
        };
        let endpoint = endpoint.ok_or(OnvifError::Response)?;
        let namespace = media_namespace(media);
        let request = format!(
            "<m:GetStreamUri>{setup}<m:ProfileToken>{}</m:ProfileToken></m:GetStreamUri>",
            escape_xml(token)
        );
        let response = self
            .soap(&endpoint, credentials, namespace, "GetStreamUri", &request)
            .await?;
        let doc = parse_xml(&response).ok_or(OnvifError::Response)?;
        let response = soap_result(&doc, namespace, "GetStreamUriResponse")?;
        let uri = match media {
            OnvifMedia::Media1 => {
                child(response, MEDIA1, "MediaUri").and_then(|node| child(node, TYPES, "Uri"))
            }
            OnvifMedia::Media2 => child(response, MEDIA2, "Uri"),
        };
        uri.and_then(|node| node.text())
            .and_then(|uri| stream_source(uri.trim(), numeric_host(device)?, credential_ref))
            .ok_or(OnvifError::Response)
    }

    async fn services(
        &self,
        device: &Url,
        credentials: Option<&CameraCredentials>,
    ) -> Result<(Option<Url>, Option<Url>), OnvifError> {
        match self
            .soap(
                device,
                credentials,
                DEVICE,
                "GetServices",
                "<m:GetServices><m:IncludeCapability>false</m:IncludeCapability></m:GetServices>",
            )
            .await
        {
            Ok(body) => {
                let doc = parse_xml(&body).ok_or(OnvifError::Response)?;
                if let Ok(response) = soap_result(&doc, DEVICE, "GetServicesResponse") {
                    let find = |namespace| {
                        response
                            .children()
                            .filter(|node| node.has_tag_name((DEVICE, "Service")))
                            .filter(|node| {
                                child(*node, DEVICE, "Namespace")
                                    .and_then(|n| n.text())
                                    .map(str::trim)
                                    == Some(namespace)
                            })
                            .filter_map(|node| child(node, DEVICE, "XAddr")?.text())
                            .find_map(|value| same_origin_endpoint(value.trim(), device))
                    };
                    let services = (find(MEDIA2), find(MEDIA1));
                    if services.0.is_some() || services.1.is_some() {
                        return Ok(services);
                    }
                }
            }
            Err(OnvifError::Response) => {}
            Err(error) => return Err(error),
        }
        // Older device implementations expose Media1 through GetCapabilities.
        let body = self
            .soap(
                device,
                credentials,
                DEVICE,
                "GetCapabilities",
                "<m:GetCapabilities><m:Category>Media</m:Category></m:GetCapabilities>",
            )
            .await?;
        let doc = parse_xml(&body).ok_or(OnvifError::Response)?;
        let response = soap_result(&doc, DEVICE, "GetCapabilitiesResponse")?;
        let media1 = child(response, DEVICE, "Capabilities")
            .and_then(|node| child(node, TYPES, "Media"))
            .and_then(|node| child(node, TYPES, "XAddr"))
            .and_then(|node| node.text())
            .and_then(|value| same_origin_endpoint(value.trim(), device))
            .ok_or(OnvifError::Response)?;
        Ok((None, Some(media1)))
    }

    async fn soap(
        &self,
        url: &Url,
        credentials: Option<&CameraCredentials>,
        namespace: &str,
        operation: &str,
        body: &str,
    ) -> Result<Vec<u8>, OnvifError> {
        let envelope = |security: &str| {
            format!(
                r#"<s:Envelope xmlns:s="{SOAP}" xmlns:m="{namespace}" xmlns:tt="{TYPES}">{security}<s:Body>{body}</s:Body></s:Envelope>"#
            )
        };
        let send = |payload: String| {
            self.client
                .post(url.clone())
                .header(
                    header::CONTENT_TYPE,
                    format!(
                        "application/soap+xml; charset=utf-8; action=\"{namespace}/{operation}\""
                    ),
                )
                .body(payload)
        };
        let payload = envelope("");
        let mut response = send(payload.clone())
            .send()
            .await
            .map_err(|_| OnvifError::Unavailable)?;
        let status = response.status();
        if let Some(credentials) = credentials {
            if status == StatusCode::UNAUTHORIZED
                && let Some(auth) = digest_response(response.headers(), url, &payload, credentials)?
            {
                response = send(payload)
                    .header(header::AUTHORIZATION, auth)
                    .send()
                    .await
                    .map_err(|_| OnvifError::Unavailable)?;
                return read_response(response).await;
            }
            // Legacy devices often signal a missing UsernameToken as a SOAP fault.
            if matches!(
                status,
                StatusCode::UNAUTHORIZED
                    | StatusCode::FORBIDDEN
                    | StatusCode::INTERNAL_SERVER_ERROR
            ) {
                drop(response);
                response = send(envelope(&username_token(credentials)?))
                    .send()
                    .await
                    .map_err(|_| OnvifError::Unavailable)?;
            }
        }
        read_response(response).await
    }
}

fn media_namespace(media: OnvifMedia) -> &'static str {
    match media {
        OnvifMedia::Media1 => MEDIA1,
        OnvifMedia::Media2 => MEDIA2,
    }
}

fn parse_xml(bytes: &[u8]) -> Option<Document<'_>> {
    let text = std::str::from_utf8(bytes).ok()?;
    let options = ParsingOptions {
        allow_dtd: false,
        nodes_limit: 8192,
        ..Default::default()
    };
    Document::parse_with_options(text, options).ok()
}

fn child<'a, 'input>(node: Node<'a, 'input>, ns: &str, name: &str) -> Option<Node<'a, 'input>> {
    node.children().find(|item| item.has_tag_name((ns, name)))
}

fn soap_result<'a, 'input>(
    doc: &'a Document<'input>,
    ns: &str,
    name: &str,
) -> Result<Node<'a, 'input>, OnvifError> {
    let root = doc.root_element();
    if root.has_tag_name((SOAP, "Envelope"))
        && let Some(node) = child(root, SOAP, "Body").and_then(|body| child(body, ns, name))
    {
        return Ok(node);
    }
    Err(OnvifError::Response)
}

fn probe_matches(
    bytes: &[u8],
    id: &str,
    sender: IpAddr,
    allowed: &HashSet<Url>,
) -> Vec<OnvifDevice> {
    let Some(doc) = parse_xml(bytes) else {
        return Vec::new();
    };
    let Some(header) = child(doc.root_element(), SOAP, "Header") else {
        return Vec::new();
    };
    let relates = child(header, DISCOVERY_ADDRESSING, "RelatesTo").and_then(|node| node.text());
    let action = child(header, DISCOVERY_ADDRESSING, "Action").and_then(|node| node.text());
    if relates.map(str::trim) != Some(id)
        || action.map(str::trim)
            != Some(concat!(
                "http://schemas.xmlsoap.org/ws/2005/04/discovery",
                "/ProbeMatches"
            ))
    {
        return Vec::new();
    }
    let Ok(matches) = soap_result(&doc, DISCOVERY, "ProbeMatches") else {
        return Vec::new();
    };
    matches
        .children()
        .filter(|node| node.has_tag_name((DISCOVERY, "ProbeMatch")))
        .filter_map(|node| child(node, DISCOVERY, "XAddrs")?.text())
        .flat_map(str::split_ascii_whitespace)
        .filter_map(endpoint)
        .filter(|url| numeric_host(url) == Some(sender))
        .take(MAX_RESULTS)
        .map(|url| OnvifDevice {
            authorized: allowed.contains(&url),
            endpoint: url.into(),
        })
        .collect()
}

fn numeric_host(url: &Url) -> Option<IpAddr> {
    let ip = url
        .host_str()?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<IpAddr>()
        .ok()?;
    match ip {
        IpAddr::V4(addr) if addr.is_broadcast() || addr.is_link_local() => None,
        IpAddr::V6(addr) if addr.is_unicast_link_local() || addr.to_ipv4_mapped().is_some() => None,
        _ if ip.is_unspecified() || ip.is_multicast() => None,
        _ => Some(ip),
    }
}

fn checked_url(value: &str) -> Option<Url> {
    if value.len() > 768
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && byte != b'\\')
    {
        return None;
    }
    let authority = value.split_once("://")?.1.split(['/', '?', '#']).next()?;
    if authority.contains('@') {
        return None;
    }
    let url = Url::parse(value).ok()?;
    numeric_host(&url)?;
    (url.fragment().is_none() && url.port() != Some(0)).then_some(url)
}

fn endpoint(value: &str) -> Option<Url> {
    let url = checked_url(value)?;
    (matches!(url.scheme(), "http" | "https")
        && url.query().is_none()
        && url.path().len() > 1
        && url
            .path()
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"/-_.~".contains(&byte)))
    .then_some(url)
}

fn same_origin_endpoint(value: &str, device: &Url) -> Option<Url> {
    endpoint(value).filter(|url| url.origin() == device.origin())
}

fn stream_source(
    value: &str,
    device: IpAddr,
    credential_ref: Option<Uuid>,
) -> Option<CameraSource> {
    let url = checked_url(value)?;
    if numeric_host(&url)? != device {
        return None;
    }
    let (secure, port) = match url.scheme() {
        "rtsp" => (false, 554),
        "rtsps" => (true, 322),
        _ => return None,
    };
    let source = CameraSource::Rtsp {
        peer: SocketAddr::new(device, url.port().unwrap_or(port)),
        stream_path: url.query().map_or_else(
            || url.path().to_owned(),
            |query| format!("{}?{query}", url.path()),
        ),
        secure,
        transport: RtspTransport::Tcp,
        credential_ref,
    };
    source.valid().then_some(source)
}

fn valid_token(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn digest_response(
    headers: &header::HeaderMap,
    url: &Url,
    payload: &str,
    credentials: &CameraCredentials,
) -> Result<Option<header::HeaderValue>, OnvifError> {
    let Some(mut challenge) = headers
        .get_all(header::WWW_AUTHENTICATE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .filter(|value| value.len() <= 4096 && value.starts_with("Digest "))
        .filter_map(|value| digest_auth::parse(value).ok())
        .max_by_key(|challenge| match challenge.algorithm.algo {
            AlgorithmType::MD5 => 0,
            AlgorithmType::SHA2_256 => 1,
            AlgorithmType::SHA2_512_256 => 2,
        })
    else {
        return Ok(None);
    };
    let context = AuthContext::new_post(
        credentials.username.as_str(),
        credentials.password.as_str(),
        url.path(),
        Some(payload.as_bytes()),
    );
    let response = challenge
        .respond(&context)
        .map_err(|_| OnvifError::Authentication)?;
    let mut value = header::HeaderValue::from_str(&response.to_string())
        .map_err(|_| OnvifError::Authentication)?;
    value.set_sensitive(true);
    Ok(Some(value))
}

fn username_token(credentials: &CameraCredentials) -> Result<String, OnvifError> {
    const WSSE: &str =
        "http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-secext-1.0.xsd";
    const WSU: &str =
        "http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-utility-1.0.xsd";
    const PASSWORD_DIGEST: &str = "http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-username-token-profile-1.0#PasswordDigest";
    const NONCE_ENCODING: &str = "http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-soap-message-security-1.0#Base64Binary";
    let created = OffsetDateTime::now_utc()
        .replace_nanosecond(0)
        .map_err(|_| OnvifError::Unavailable)?
        .format(&Rfc3339)
        .map_err(|_| OnvifError::Unavailable)?;
    let mut nonce = [0; 20];
    getrandom::fill(&mut nonce).map_err(|_| OnvifError::Unavailable)?;
    let mut digest = ring::digest::Context::new(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY);
    digest.update(&nonce);
    digest.update(created.as_bytes());
    digest.update(credentials.password.as_bytes());
    let digest = STANDARD.encode(digest.finish());
    let nonce = STANDARD.encode(nonce);
    let username = escape_xml(&credentials.username);
    Ok(format!(
        r#"<s:Header><wsse:Security xmlns:wsse="{WSSE}" xmlns:wsu="{WSU}"><wsse:UsernameToken><wsse:Username>{username}</wsse:Username><wsse:Password Type="{PASSWORD_DIGEST}">{digest}</wsse:Password><wsse:Nonce EncodingType="{NONCE_ENCODING}">{nonce}</wsse:Nonce><wsu:Created>{created}</wsu:Created></wsse:UsernameToken></wsse:Security></s:Header>"#
    ))
}

async fn read_response(mut response: reqwest::Response) -> Result<Vec<u8>, OnvifError> {
    if matches!(
        response.status(),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
    ) {
        return Err(OnvifError::Authentication);
    }
    if !response.status().is_success()
        || response
            .content_length()
            .is_some_and(|length| length > MAX_SOAP_BYTES as u64)
    {
        return Err(OnvifError::Response);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| OnvifError::Unavailable)?
    {
        if body.len() + chunk.len() > MAX_SOAP_BYTES {
            return Err(OnvifError::Response);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}
