#![forbid(unsafe_code)]

use std::{
    fs,
    io::Read,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow, ensure};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode, header},
    routing::post,
};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use clap::{Parser, ValueEnum};
use ring::{
    rand::SystemRandom,
    signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair},
};
use serde_json::json;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use sw_protocol::{ApnsDelivery, IosPushRegistration};
use tokio::{
    net::TcpListener,
    sync::{Mutex, Semaphore},
};

const APP_TOPIC: &str = "com.stillwatch.stillwatchCompanion";
const TOKEN_REFRESH_SECONDS: u64 = 50 * 60;

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "127.0.0.1:8776")]
    listen: SocketAddr,
    #[arg(long)]
    delivery_token_file: PathBuf,
    #[arg(long)]
    apple_key_file: PathBuf,
    #[arg(long)]
    team_id: String,
    #[arg(long)]
    key_id: String,
    #[arg(long, value_enum)]
    environment: Environment,
}

#[derive(Clone, Copy, ValueEnum)]
enum Environment {
    Sandbox,
    Production,
}

impl Environment {
    fn endpoint(self) -> &'static str {
        match self {
            Self::Sandbox => "https://api.sandbox.push.apple.com/3/device/",
            Self::Production => "https://api.push.apple.com/3/device/",
        }
    }
}

struct Gateway {
    host: String,
    inbound_digest: [u8; 32],
    client: reqwest::Client,
    endpoint: &'static str,
    signer: Signer,
    jwt: Mutex<Option<(u64, String)>>,
    slots: Semaphore,
}

struct Signer {
    key: EcdsaKeyPair,
    team: String,
    kid: String,
}

impl Signer {
    fn read(team: String, kid: String, path: &Path) -> Result<Self> {
        for value in [&team, &kid] {
            ensure!(
                value.len() == 10
                    && value
                        .bytes()
                        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit()),
                "invalid APNs key or Team ID"
            );
        }
        let pem = read_private(path, 4096)?;
        let encoded = pem
            .trim()
            .strip_prefix("-----BEGIN PRIVATE KEY-----") // gitleaks:allow Standard PEM header.
            .and_then(|value| value.strip_suffix("-----END PRIVATE KEY-----"))
            .context("expected an APNs PKCS#8 PEM key")?;
        let mut key_der = STANDARD
            .decode(encoded.split_ascii_whitespace().collect::<String>())
            .map_err(|_| anyhow!("invalid APNs PKCS#8 PEM encoding"))?;
        let key = EcdsaKeyPair::from_pkcs8(
            &ECDSA_P256_SHA256_FIXED_SIGNING,
            &key_der,
            &SystemRandom::new(),
        );
        key_der.fill(0);
        Ok(Self {
            key: key.map_err(|_| anyhow!("APNs key must be a P-256 signing key"))?,
            team,
            kid,
        })
    }

    fn token(&self, seconds: u64) -> Result<String> {
        let header = URL_SAFE_NO_PAD.encode(serde_json::to_vec(
            &json!({"alg": "ES256", "kid": self.kid}),
        )?);
        let claims = URL_SAFE_NO_PAD.encode(serde_json::to_vec(
            &json!({"iss": self.team, "iat": seconds}),
        )?);
        let prefix = format!("{header}.{claims}");
        let signature = self
            .key
            .sign(&SystemRandom::new(), prefix.as_bytes())
            .map_err(|_| anyhow!("APNs signing failed"))?;
        Ok(format!(
            "{prefix}.{}",
            URL_SAFE_NO_PAD.encode(signature.as_ref())
        ))
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(
        args.listen.ip().is_loopback() && args.listen.port() > 0,
        "APNs worker requires a loopback listener"
    );
    let secret = read_private(&args.delivery_token_file, 66)?;
    let secret = secret.trim();
    ensure!(
        secret.len() == 64 && secret.bytes().all(|b| b.is_ascii_hexdigit()),
        "APNs delivery credential must be 32 random bytes in hexadecimal"
    );
    let _ = rustls::crypto::ring::default_provider().install_default();
    let gateway = Gateway {
        host: args.listen.to_string(),
        inbound_digest: Sha256::digest(secret.as_bytes()).into(),
        client: reqwest::Client::builder()
            .https_only(true)
            .no_proxy()
            .http2_prior_knowledge()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(4))
            .build()
            .context("APNs TLS configuration")?,
        endpoint: args.environment.endpoint(),
        signer: Signer::read(args.team_id, args.key_id, &args.apple_key_file)?,
        jwt: Mutex::new(None),
        slots: Semaphore::new(4),
    };
    let router = Router::new()
        .route("/deliver", post(deliver))
        .layer(DefaultBodyLimit::max(2048))
        .with_state(Arc::new(gateway));
    let listener = TcpListener::bind(args.listen)
        .await
        .context("bind APNs loopback listener")?;
    eprintln!("StillWatch APNs worker listening on {}", args.listen);
    axum::serve(listener, router)
        .await
        .context("APNs worker listener")
}

async fn deliver(
    State(state): State<Arc<Gateway>>,
    headers: HeaderMap,
    Json(delivery): Json<ApnsDelivery>,
) -> StatusCode {
    if headers.contains_key(header::ORIGIN)
        || headers
            .get(header::HOST)
            .and_then(|host| host.to_str().ok())
            != Some(&state.host)
    {
        return StatusCode::FORBIDDEN;
    }
    let supplied = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .filter(|value| value.len() == 64);
    if !supplied.is_some_and(|value| {
        bool::from(
            Sha256::digest(value.as_bytes())
                .as_slice()
                .ct_eq(&state.inbound_digest),
        )
    }) {
        return StatusCode::UNAUTHORIZED;
    }
    let Ok(_permit) = state.slots.try_acquire() else {
        return StatusCode::TOO_MANY_REQUESTS;
    };
    let Ok(now) = SystemTime::now().duration_since(UNIX_EPOCH) else {
        return StatusCode::SERVICE_UNAVAILABLE;
    };
    let expiry = delivery.expires_at.unix_millis().div_euclid(1_000);
    if !IosPushRegistration::valid_token(&delivery.token)
        || delivery.notice.is_nil()
        || expiry <= now.as_secs() as i64
        || expiry > now.as_secs() as i64 + 3600
    {
        return StatusCode::UNPROCESSABLE_ENTITY;
    }
    let credential = {
        let mut cached = state.jwt.lock().await;
        let seconds = now.as_secs();
        if cached.as_ref().is_none_or(|(issued, _)| {
            seconds < *issued || seconds - issued >= TOKEN_REFRESH_SECONDS
        }) {
            let Ok(new) = state.signer.token(seconds) else {
                return StatusCode::SERVICE_UNAVAILABLE;
            };
            *cached = Some((seconds, new));
        }
        let Some((_, jwt)) = cached.as_ref() else {
            return StatusCode::SERVICE_UNAVAILABLE;
        };
        jwt.clone()
    };
    let notice = delivery.notice.to_string();
    let payload = json!({
        "aps": {"alert": {"title": "StillWatch alert", "body": "Open StillWatch to review an urgent incident."}, "sound": "default"},
        "stillwatch_notice": notice,
    });
    let result = state
        .client
        .post(format!("{}{}", state.endpoint, delivery.token))
        .bearer_auth(credential)
        .header(header::CONTENT_TYPE, "application/json")
        .header("apns-topic", APP_TOPIC)
        .header("apns-push-type", "alert")
        .header("apns-priority", "10")
        .header("apns-id", &notice)
        .header("apns-expiration", expiry.to_string())
        .header("apns-collapse-id", &notice)
        .body(payload.to_string())
        .send()
        .await;
    match result.map(|response| response.status()) {
        Ok(StatusCode::OK) => StatusCode::NO_CONTENT,
        Ok(StatusCode::GONE) => StatusCode::GONE,
        _ => StatusCode::SERVICE_UNAVAILABLE,
    }
}

fn read_private(path: &Path, maximum: u64) -> Result<String> {
    let metadata = fs::symlink_metadata(path).context("read private APNs file metadata")?;
    ensure!(
        metadata.is_file() && metadata.len() <= maximum,
        "invalid private APNs file"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            metadata.permissions().mode() & 0o077 == 0,
            "APNs file permissions must be private"
        );
    }
    let mut value = String::new();
    fs::File::open(path)?
        .take(maximum + 1)
        .read_to_string(&mut value)?;
    ensure!(
        value.len() as u64 <= maximum,
        "APNs file changed while reading"
    );
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::{ECDSA_P256_SHA256_FIXED, KeyPair, UnparsedPublicKey};

    #[test]
    fn jwt_uses_fixed_width_es256_signature_and_exact_provider_claims() -> Result<()> {
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng)
            .map_err(|_| anyhow!("generate test signing key"))?;
        let signer = Signer {
            key: EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng)
                .map_err(|_| anyhow!("parse test signing key"))?,
            team: "T234567890".into(),
            kid: "K234567890".into(),
        };
        let jwt = signer.token(1_795_000_000)?;
        let (signed, signature) = jwt.rsplit_once('.').context("JWT signature")?;
        let (header, claims) = signed.split_once('.').context("JWT claims")?;
        let signature = URL_SAFE_NO_PAD.decode(signature)?;
        assert_eq!(signature.len(), 64);
        UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, signer.key.public_key().as_ref())
            .verify(signed.as_bytes(), &signature)
            .map_err(|_| anyhow!("verify test JWT signature"))?;
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&URL_SAFE_NO_PAD.decode(header)?)?,
            json!({"alg":"ES256","kid":"K234567890"})
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&URL_SAFE_NO_PAD.decode(claims)?)?,
            json!({"iss":"T234567890","iat":1795000000})
        );
        Ok(())
    }
}
