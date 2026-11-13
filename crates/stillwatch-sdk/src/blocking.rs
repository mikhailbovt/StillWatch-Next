use std::time::Duration;

use serde::de::DeserializeOwned;
use sw_protocol::{Bootstrap, Health, Snapshot};
use uuid::Uuid;

use crate::{Error, PairedEndpoint, endpoint_url, valid_token};

pub struct Client {
    endpoint: reqwest::Url,
    http: reqwest::blocking::Client,
    token: String,
}

impl Client {
    pub fn loopback(endpoint: &str, token: String) -> Result<Self, Error> {
        Self::new(endpoint_url(endpoint, false)?, token, None)
    }

    pub fn pinned(endpoint: &PairedEndpoint, token: String) -> Result<Self, Error> {
        Self::new(endpoint.url.clone(), token, Some(&endpoint.der))
    }

    fn new(endpoint: reqwest::Url, token: String, cert: Option<&[u8]>) -> Result<Self, Error> {
        if !valid_token(&token) {
            return Err(Error::Credentials);
        }
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut builder = reqwest::blocking::Client::builder()
            .no_proxy()
            .tls_certs_only([])
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(10));
        if let Some(der) = cert {
            let cert = reqwest::Certificate::from_der(der).map_err(|_| Error::Endpoint)?;
            builder = builder.https_only(true).tls_certs_only([cert]);
        }
        Ok(Self {
            endpoint,
            token,
            http: builder.build().map_err(Error::transport)?,
        })
    }

    pub fn capabilities(&self) -> Result<Bootstrap, Error> {
        self.get("/bootstrap/info")
    }
    pub fn readiness(&self) -> Result<Health, Error> {
        self.get("/health/ready")
    }
    pub fn status(&self) -> Result<sw_protocol::CoreStatus, Error> {
        self.get("/api/v2/status")
    }
    pub fn snapshot(&self) -> Result<Snapshot, Error> {
        self.get("/api/v2/snapshot")
    }
    pub fn camera(&self, id: Uuid) -> Result<sw_protocol::Camera, Error> {
        self.get(&format!("/api/v2/cameras/{id}"))
    }
    pub fn incident(&self, id: Uuid) -> Result<sw_protocol::Incident, Error> {
        self.get(&format!("/api/v2/incidents/{id}"))
    }

    fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, Error> {
        use std::io::Read;
        let url = self.endpoint.join(path).map_err(|_| Error::Endpoint)?;
        let response = self
            .http
            .get(url)
            .bearer_auth(&self.token)
            .send()
            .map_err(Error::transport)?;
        let status = response.status().as_u16();
        let limit = if status < 300 {
            super::RESPONSE_LIMIT
        } else {
            16 * 1024
        };
        let mut bytes = Vec::new();
        response
            .take(limit as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| Error::Transport)?;
        if bytes.len() > limit {
            return Err(Error::Protocol);
        }
        if (200..300).contains(&status) {
            serde_json::from_slice(&bytes).map_err(|_| Error::Protocol)
        } else {
            let problem = serde_json::from_slice::<sw_protocol::Problem>(&bytes)
                .ok()
                .filter(|p| p.status == status)
                .map(Box::new);
            Err(Error::Http { status, problem })
        }
    }
}
