use std::{io, net::SocketAddr, path::Path, time::Duration};

use reqwest::{Client, StatusCode};
use sw_storage::{ApnsOutcome, Store};
use tokio::{io::AsyncReadExt, sync::watch};

pub struct ApnsSender {
    client: Client,
    url: String,
    credential: String,
    store: Store,
}

impl ApnsSender {
    pub async fn new(
        address: SocketAddr,
        credential_file: &Path,
        store: Store,
    ) -> io::Result<Self> {
        if !address.ip().is_loopback() || address.port() == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "APNs worker must listen on loopback",
            ));
        }
        let metadata = tokio::fs::symlink_metadata(credential_file).await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "APNs credential must be private",
                ));
            }
        }
        if !metadata.is_file() || metadata.len() > 66 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "APNs credential file is invalid",
            ));
        }
        let mut value = String::new();
        tokio::fs::File::open(credential_file)
            .await?
            .take(67)
            .read_to_string(&mut value)
            .await?;
        let credential = value.trim();
        if credential.len() != 64 || !credential.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "APNs credential must be 32 random bytes in hexadecimal",
            ));
        }
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(1))
            .timeout(Duration::from_secs(5))
            .build()
            .map_err(io::Error::other)?;
        Ok(Self {
            client,
            url: format!("http://{address}/deliver"),
            credential: credential.to_owned(),
            store,
        })
    }

    pub async fn run(self, mut shutdown: watch::Receiver<bool>) {
        while !*shutdown.borrow() {
            let delay = self.dispatch().await;
            tokio::select! {
                _ = tokio::time::sleep(delay) => {},
                _ = shutdown.wait_for(|stopping| *stopping) => break,
            }
        }
    }

    async fn dispatch(&self) -> Duration {
        let Some(now) = utc_ms() else {
            return Duration::from_secs(2);
        };
        let delivery = match self.store.take_apns_delivery(now).await {
            Ok(Some(delivery)) => delivery,
            Ok(None) => return Duration::from_secs(1),
            Err(_) => return Duration::from_secs(2),
        };
        let Ok(body) = serde_json::to_vec(&delivery.envelope) else {
            return Duration::from_secs(2);
        };
        let response = self
            .client
            .post(&self.url)
            .bearer_auth(&self.credential)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await;
        let (outcome, delay) = match response.map(|response| response.status()) {
            Ok(StatusCode::NO_CONTENT) => (ApnsOutcome::Accepted, Duration::from_millis(200)),
            Ok(StatusCode::GONE) => (ApnsOutcome::Unregistered, Duration::from_millis(200)),
            _ => (ApnsOutcome::Retry, Duration::from_secs(2)),
        };
        let _ = self
            .store
            .finish_apns_delivery(delivery, utc_ms().unwrap_or(now), outcome)
            .await;
        delay
    }
}

fn utc_ms() -> Option<i64> {
    let elapsed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    elapsed.as_millis().try_into().ok()
}
