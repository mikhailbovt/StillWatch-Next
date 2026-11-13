mod private_file;
mod sound;

use std::{
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use stillwatch_sdk::{
    Client, DeviceKey, DeviceSession, Error, PairedEndpoint, SessionPersistence, enroll,
    protocol::{AlertDelivery, Attention, Counter, DeviceCredentials, PairingOffer},
};
use uuid::Uuid;

#[derive(Parser)]
#[command(version, about = "StillWatch desktop session alarm agent")]
struct Options {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Register this computer. Pipe a one-time pairing offer into stdin.
    Enroll {
        #[arg(long)]
        state: PathBuf,
        #[arg(long)]
        label: String,
    },
    /// Watch active alerts in the current user session.
    Run {
        #[arg(long)]
        state: PathBuf,
        /// Enable the local sound for an urgent alert.
        #[arg(long)]
        audible: bool,
    },
    /// Check an enrolled Core from a separate host and report its availability.
    Monitor {
        #[arg(long)]
        state: PathBuf,
        #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u64).range(2..=300))]
        interval_seconds: u64,
        #[arg(long, default_value_t = 3, value_parser = clap::value_parser!(u8).range(1..=10))]
        consecutive_failures: u8,
    },
    /// Play a brief test tone on the current audio output.
    TestSound,
}

#[derive(Serialize, Deserialize)]
struct State {
    endpoint: String,
    instance: Uuid,
    digest: String,
    certificate: String,
    seed: [u8; 32],
    credentials: DeviceCredentials,
}

struct Persist(PathBuf);

impl SessionPersistence for Persist {
    fn save<'a>(
        &'a self,
        credentials: &'a DeviceCredentials,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<(), Error>> + Send + 'a>> {
        Box::pin(async move {
            let mut state = read_state(&self.0).map_err(|_| Error::Credentials)?;
            state.credentials = serde_json::from_slice(
                &serde_json::to_vec(credentials).map_err(|_| Error::Credentials)?,
            )
            .map_err(|_| Error::Credentials)?;
            write_state(&self.0, &state).map_err(|_| Error::Credentials)
        })
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    match Options::parse().command {
        Command::Enroll { state, label } => {
            let _lock = lock(&state)?;
            ensure!(!state.exists(), "alarm state already exists");
            let mut input = Vec::new();
            io::stdin().take(8193).read_to_end(&mut input)?;
            ensure!(input.len() <= 8192, "pairing offer exceeds the limit");
            let offer: PairingOffer =
                serde_json::from_slice(&input).context("invalid pairing offer")?;
            let endpoint = offer.endpoint.clone();
            let instance = offer.core_instance_id;
            let digest = offer.tls_certificate_sha256.clone();
            let certificate = offer.tls_certificate_der.clone();
            let key = DeviceKey::generate()?;
            let (_, credentials) = enroll(offer, &key, label).await?;
            write_state(
                &state,
                &State {
                    endpoint,
                    instance,
                    digest,
                    certificate,
                    seed: key.export_seed(),
                    credentials,
                },
            )?;
            println!("Alarm agent paired. Start it in this user session.");
            Ok(())
        }
        Command::Run { state, audible } => {
            let _lock = lock(&state)?;
            let (client, instance) = open_client(state)?;
            watch(client, instance, audible).await
        }
        Command::Monitor {
            state,
            interval_seconds,
            consecutive_failures,
        } => {
            let _lock = lock(&state)?;
            let (client, instance) = open_client(state)?;
            monitor(
                client,
                instance,
                Duration::from_secs(interval_seconds),
                consecutive_failures,
            )
            .await
        }
        Command::TestSound => sound::Output::new()?.play().await,
    }
}

fn open_client(state: PathBuf) -> Result<(Client, Uuid)> {
    let saved = read_state(&state)?;
    let endpoint = PairedEndpoint::new(
        &saved.endpoint,
        saved.instance,
        &saved.digest,
        &saved.certificate,
    )?;
    let session = DeviceSession::new(
        &endpoint,
        DeviceKey::from_seed(&saved.seed),
        saved.credentials,
        Some(Arc::new(Persist(state))),
    )?;
    Ok((
        Client::pinned(&endpoint, Arc::new(session))?,
        endpoint.instance(),
    ))
}

async fn monitor(client: Client, core: Uuid, period: Duration, threshold: u8) -> Result<()> {
    let mut ticks = tokio::time::interval(period);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let stop = tokio::signal::ctrl_c();
    tokio::pin!(stop);
    let mut last = None;
    let mut failures = 0u8;
    loop {
        tokio::select! {
            result = &mut stop => return result.context("heartbeat monitor shutdown"),
            _ = ticks.tick() => {}
        }
        let probe = async {
            client.capabilities().await?;
            client.readiness().await
        };
        let result = tokio::select! {
            result = &mut stop => return result.context("heartbeat monitor shutdown"),
            result = probe => result,
        };
        let state = match result {
            Ok(_) => {
                failures = 0;
                "ready"
            }
            Err(Error::Credentials | Error::Identity) => {
                anyhow::bail!("heartbeat monitor credentials require re-enrollment")
            }
            Err(error) if error.problem().is_some_and(|problem| problem.status == 401) => {
                anyhow::bail!("heartbeat monitor authorization was revoked")
            }
            Err(error) => {
                failures = failures.saturating_add(1);
                if failures < threshold {
                    continue;
                }
                if matches!(error, Error::Http { status: 503, .. }) {
                    "degraded"
                } else {
                    "unreachable"
                }
            }
        };
        if last != Some(state) {
            last = Some(state);
            println!(
                "{}",
                serde_json::json!({
                    "core_id": core, "state": state, "failed_probes": failures,
                    "observed_unix_ms": (time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64,
                })
            );
            io::stdout().flush()?;
        }
    }
}

async fn watch(client: Client, core: Uuid, audible: bool) -> Result<()> {
    let output = audible.then(sound::Output::new).transpose()?;
    let mut tick = tokio::time::interval(Duration::from_secs(2));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut ringing: Option<(Counter, Instant)> = None;
    let mut online = false;
    let stop = tokio::signal::ctrl_c();
    tokio::pin!(stop);
    loop {
        tokio::select! {
            result = &mut stop => return result.context("alarm agent shutdown"),
            _ = tick.tick() => {}
        }
        let poll = async {
            if !online {
                client.capabilities().await?;
            }
            client.alerts().await
        };
        let result = tokio::select! {
            result = &mut stop => return result.context("alarm agent shutdown"),
            result = poll => result,
        };
        let feed = match result {
            Ok(feed) => {
                if !online {
                    eprintln!("StillWatch alarm agent connected");
                    online = true;
                }
                feed
            }
            Err(Error::Credentials | Error::Identity) => {
                anyhow::bail!("alarm agent credentials require re-enrollment")
            }
            Err(error) if error.problem().is_some_and(|problem| problem.status == 401) => {
                anyhow::bail!("alarm agent authorization was revoked")
            }
            Err(_) => {
                if online {
                    eprintln!("StillWatch alarm agent lost its Core connection");
                }
                online = false;
                continue;
            }
        };
        let now = (time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64;
        let mut urgent = feed
            .alerts
            .iter()
            .filter(|alert| fresh_urgent(alert, core, now));
        if let Some(alert) = urgent.clone().find(|alert| alert.client_seen_at.is_none()) {
            let already_played = ringing.is_some_and(|(id, _)| id == alert.id);
            if !already_played {
                eprintln!("StillWatch urgent incident {}", alert.incident_id);
                let start = Instant::now();
                if let Some(output) = &output
                    && output.play().await.is_err()
                {
                    eprintln!("StillWatch audio output is unavailable");
                    continue;
                }
                ringing = Some((alert.id, start));
            }
            let _ = client.alert_seen(alert.id, alert.attention_cycle).await;
        } else if let Some((id, start)) = ringing
            && start.elapsed() < Duration::from_secs(30)
            && urgent.any(|alert| alert.id == id)
        {
            if let Some(output) = &output
                && output.play().await.is_err()
            {
                eprintln!("StillWatch audio output is unavailable");
            }
        } else {
            ringing = None;
        }
    }
}

fn fresh_urgent(alert: &AlertDelivery, core: Uuid, now: i64) -> bool {
    alert.core_id == core
        && alert.attention == Attention::Urgent
        && alert.expires_at.unix_millis() > now
}

fn lock(path: &Path) -> Result<fs::File> {
    let parent = path
        .parent()
        .filter(|value| !value.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.with_extension("agent-lock"))?;
    file.try_lock()
        .context("this alarm agent state is already in use")?;
    Ok(file)
}

fn read_state(path: &Path) -> Result<State> {
    let protected = private_file::read(path)?;
    let plain = private_file::decrypt(&protected)?;
    serde_json::from_slice(&plain).context("invalid alarm agent state")
}

fn write_state(path: &Path, state: &State) -> Result<()> {
    let parent = path
        .parent()
        .filter(|value| !value.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    let protected = private_file::encrypt(&serde_json::to_vec(state)?)?;
    temporary.write_all(&protected)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path)?;
    #[cfg(unix)]
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}
