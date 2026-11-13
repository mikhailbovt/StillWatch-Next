use std::sync::Arc;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signer, SigningKey};
use reqwest::{Method, Url};
use sha2::{Digest, Sha256};
use sw_protocol::{
    ApiVersion, Device, DeviceCredentials, PairingFinish, PairingOffer, RefreshRequest,
    pairing_message, refresh_message,
};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::{Client, CredentialFuture, Credentials, Error, body, endpoint_url, valid_token};

pub struct DeviceKey(SigningKey);

impl DeviceKey {
    pub fn generate() -> Result<Self, Error> {
        let mut secret = [0; 32];
        getrandom::fill(&mut secret).map_err(|_| Error::Credentials)?;
        Ok(Self(SigningKey::from_bytes(&secret)))
    }

    pub fn from_seed(seed: &[u8; 32]) -> Self {
        Self(SigningKey::from_bytes(seed))
    }

    pub fn export_seed(&self) -> [u8; 32] {
        self.0.to_bytes()
    }

    fn sign(&self, bytes: &[u8]) -> String {
        hex(&self.0.sign(bytes).to_bytes())
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[derive(Clone)]
pub struct PairedEndpoint {
    pub(crate) url: Url,
    pub(crate) der: Vec<u8>,
    instance: Uuid,
}

impl PairedEndpoint {
    pub fn from_offer(offer: &PairingOffer) -> Result<Self, Error> {
        if !offer.api.supports(ApiVersion::CURRENT) || offer.core_instance_id.is_nil() {
            return Err(Error::Identity);
        }
        Self::new(
            &offer.endpoint,
            offer.core_instance_id,
            &offer.tls_certificate_sha256,
            &offer.tls_certificate_der,
        )
    }

    pub fn new(
        endpoint: &str,
        instance: Uuid,
        digest: &str,
        certificate_der: &str,
    ) -> Result<Self, Error> {
        if digest.len() != 64 || certificate_der.len() > 8192 || instance.is_nil() {
            return Err(Error::Endpoint);
        }
        let url = endpoint_url(endpoint, true)?;
        let der = URL_SAFE_NO_PAD
            .decode(certificate_der)
            .map_err(|_| Error::Endpoint)?;
        if format!("{:x}", Sha256::digest(&der)) != digest {
            return Err(Error::Endpoint);
        }
        Ok(Self { url, der, instance })
    }

    pub fn instance(&self) -> Uuid {
        self.instance
    }
}

pub async fn enroll(
    offer: PairingOffer,
    key: &DeviceKey,
    label: String,
) -> Result<(PairedEndpoint, DeviceCredentials), Error> {
    let endpoint = PairedEndpoint::from_offer(&offer)?;
    if offer.expires_at.unix_millis() <= now_ms()
        || !valid_token(&offer.secret)
        || offer.challenge.is_nil()
    {
        return Err(Error::Credentials);
    }
    let client = Client::unauthenticated(&endpoint)?;
    client.capabilities().await?;
    let request = PairingFinish {
        challenge: offer.challenge,
        signature: key.sign(&pairing_message(
            endpoint.instance,
            offer.challenge,
            &offer.secret,
            &label,
        )),
        public_key: hex(&key.0.verifying_key().to_bytes()),
        secret: offer.secret,
        label,
    };
    let credentials: DeviceCredentials = client
        .json(
            Method::POST,
            "/api/v2/pairing/finish",
            Some(body(&request)?),
            None,
            false,
        )
        .await?;
    validate(&credentials)?;
    Ok((endpoint, credentials))
}

pub type SaveFuture<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), Error>> + Send + 'a>>;

pub trait SessionPersistence: Send + Sync {
    fn save<'a>(&'a self, credentials: &'a DeviceCredentials) -> SaveFuture<'a>;
}

struct SessionState {
    value: DeviceCredentials,
    dirty: bool,
    terminal: bool,
}

pub struct DeviceSession {
    client: Client,
    key: DeviceKey,
    instance: Uuid,
    state: Mutex<SessionState>,
    persistence: Option<Arc<dyn SessionPersistence>>,
}

impl DeviceSession {
    pub fn new(
        endpoint: &PairedEndpoint,
        key: DeviceKey,
        credentials: DeviceCredentials,
        persistence: Option<Arc<dyn SessionPersistence>>,
    ) -> Result<Self, Error> {
        validate(&credentials)?;
        Ok(Self {
            client: Client::unauthenticated(endpoint)?,
            key,
            instance: endpoint.instance,
            state: Mutex::new(SessionState {
                value: credentials,
                dirty: false,
                terminal: false,
            }),
            persistence,
        })
    }

    pub async fn device(&self) -> Device {
        self.state.lock().await.value.device.clone()
    }

    async fn access(&self, rejected: Option<&str>) -> Result<String, Error> {
        let mut state = self.state.lock().await;
        if state.terminal {
            return Err(Error::Credentials);
        }
        let expires = state.value.access_expires_at.unix_millis() <= now_ms() + 15_000;
        if expires || rejected.is_some_and(|old| old == state.value.access_token) {
            if state.value.refresh_expires_at.unix_millis() <= now_ms() {
                state.terminal = true;
                return Err(Error::Credentials);
            }
            let device = state.value.device.id;
            let nonce = Uuid::new_v4();
            let request = RefreshRequest {
                device_id: device,
                nonce,
                signature: self.key.sign(&refresh_message(
                    self.instance,
                    device,
                    nonce,
                    &state.value.refresh_token,
                )),
                refresh_token: state.value.refresh_token.clone(),
            };
            // A lost refresh response may have consumed the token. Require
            // re-enrollment rather than automatically replaying that credential.
            state.terminal = true;
            let next: DeviceCredentials = self
                .client
                .json(
                    Method::POST,
                    "/api/v2/device-session/refresh",
                    Some(body(&request)?),
                    None,
                    false,
                )
                .await?;
            validate(&next)?;
            if next.device.id != device {
                return Err(Error::Identity);
            }
            state.value = next;
            state.dirty = true;
            state.terminal = false;
        }
        if state.dirty
            && let Some(persistence) = &self.persistence
        {
            persistence.save(&state.value).await?;
            state.dirty = false;
        }
        Ok(state.value.access_token.clone())
    }
}

impl Credentials for DeviceSession {
    fn token(&self) -> CredentialFuture<'_> {
        Box::pin(self.access(None))
    }

    fn renew<'a>(&'a self, rejected: &'a str) -> CredentialFuture<'a> {
        Box::pin(self.access(Some(rejected)))
    }
}

fn validate(credentials: &DeviceCredentials) -> Result<(), Error> {
    if credentials.device.id.is_nil()
        || credentials.device.revoked_at.is_some()
        || !valid_token(&credentials.access_token)
        || !valid_token(&credentials.refresh_token)
        || credentials.refresh_expires_at.unix_millis()
            <= credentials.access_expires_at.unix_millis()
    {
        return Err(Error::Credentials);
    }
    Ok(())
}

fn now_ms() -> i64 {
    (time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64
}
