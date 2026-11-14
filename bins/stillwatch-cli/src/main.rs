#![forbid(unsafe_code)]

use std::{
    fs::File,
    io::Read,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use stillwatch_sdk::{Client, Error as SdkError, StaticToken};
use sw_protocol::{
    CameraConfig, CameraPatch, Counter, DeviceRole, DeviceScope, Health, HealthState,
    IdempotencyKey, PairingRequest, SourceProbeRequest,
};
use sw_storage::{LegacyImport, Store, StoreConfig, import_legacy, restore_backup};
use uuid::Uuid;

#[derive(Parser)]
#[command(version, about = "StillWatch maintenance CLI")]
struct Options {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Check Core readiness and get redacted diagnostics.
    Doctor {
        #[command(flatten)]
        host: Host,
        /// Save the same redacted JSON as a new diagnostic file.
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// List, probe, or change camera settings from the Core host.
    Cameras {
        #[command(flatten)]
        host: Host,
        #[command(subcommand)]
        action: Cameras,
    },
    /// Issue a five-minute device enrollment from the local Core.
    Pair {
        #[arg(long)]
        data_dir: PathBuf,
        #[arg(long, default_value = "127.0.0.1:8765")]
        local: SocketAddr,
        #[arg(long, value_enum)]
        role: Role,
        #[arg(long)]
        camera: Vec<Uuid>,
        #[arg(long, value_enum)]
        scope: Vec<Scope>,
        /// Output the pairing offer as JSON for manual transfer.
        #[arg(long)]
        json: bool,
        /// Issue an offer for the configured HTTPS reverse proxy.
        #[arg(long)]
        proxy: bool,
    },
    /// Back up a stopped Core's SQLite and evidence directory.
    BackupOffline {
        #[arg(long)]
        data_dir: PathBuf,
        #[arg(long)]
        destination: PathBuf,
    },
    /// Verify and restore a backup to a fresh directory.
    Restore {
        #[arg(long)]
        backup: PathBuf,
        #[arg(long)]
        destination: PathBuf,
    },
    /// Read an offline schema-1 snapshot into a separate Core data directory.
    Migrate {
        #[arg(long)]
        legacy_data: PathBuf,
        #[arg(long)]
        destination: PathBuf,
        /// JSON with the mapped first camera and optional same_geometry confirmation.
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(clap::Args)]
struct Host {
    #[arg(long)]
    data_dir: PathBuf,
    #[arg(long, default_value = "127.0.0.1:8765")]
    local: SocketAddr,
}

#[derive(Subcommand)]
enum Cameras {
    List {
        #[arg(long)]
        after: Option<Uuid>,
    },
    Devices,
    Probe {
        #[arg(long)]
        config: PathBuf,
    },
    Add {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        key: IdempotencyKey,
    },
    Update {
        id: Uuid,
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        revision: Counter,
        #[arg(long)]
        key: IdempotencyKey,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum Role {
    Owner,
    Operator,
    Viewer,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum Scope {
    EventsAck,
    EvidenceRead,
    StreamsRead,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    match Options::parse().command {
        Command::Doctor { host, output } => {
            let client = local_client(&host.data_dir, host.local)?;
            let core = client.capabilities().await?;
            let health = match client.readiness().await {
                Ok(health) => health,
                Err(SdkError::Http { status: 503, .. }) => Health {
                    state: HealthState::Degraded,
                    api: core.version,
                },
                Err(error) => return Err(error.into()),
            };
            let semantic = client.semantic_status().await?;
            let status = match client.status().await {
                Ok(status) => Some(status),
                Err(SdkError::Http { status: 404, .. }) => None,
                Err(error) => return Err(error.into()),
            };
            let active_host_health = match client.health_events(None, 200, true).await {
                Ok(page) => Some(
                    page.events
                        .into_iter()
                        .filter(|event| event.camera_id.is_none())
                        .map(|event| event.code)
                        .collect::<Vec<_>>(),
                ),
                Err(SdkError::Http {
                    status: 404 | 503 | 507,
                    ..
                }) => None,
                Err(error) => return Err(error.into()),
            };
            let mut cursor = None;
            let mut cameras = Vec::new();
            loop {
                let page = client.cameras(cursor, 100).await?;
                for camera in page
                    .cameras
                    .into_iter()
                    .filter(|camera| camera.config.enabled)
                {
                    cameras.push(serde_json::json!({
                        "camera": cameras.len() + 1, "live": camera.live, "runtime": camera.runtime,
                        "analysis": client.analysis(camera.id).await?,
                    }));
                }
                let Some(next) = page.next else { break };
                cursor = Some(next);
            }
            let report = serde_json::to_string_pretty(&serde_json::json!({
                "core_version": core.version, "cli_version": env!("CARGO_PKG_VERSION"),
                "readiness": health, "status": status, "semantic": semantic, "active_cameras": cameras,
                "active_host_health": active_host_health,
            }))?;
            if let Some(path) = output {
                use std::io::Write;
                let mut options = std::fs::OpenOptions::new();
                options.create_new(true).write(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                let mut file = options.open(path).context("create diagnostic file")?;
                writeln!(file, "{report}")?;
            } else {
                println!("{report}");
            }
        }
        Command::Cameras { host, action } => {
            let client = local_client(&host.data_dir, host.local)?;
            let result = match action {
                Cameras::List { after } => serde_json::to_value(client.cameras(after, 100).await?)?,
                Cameras::Devices => serde_json::to_value(client.capture_devices().await?)?,
                Cameras::Probe { config } => {
                    let request: SourceProbeRequest = read_json(&config)?;
                    serde_json::to_value(client.probe_source(&request).await?)?
                }
                Cameras::Add { config, key } => {
                    let config: CameraConfig = read_json(&config)?;
                    serde_json::to_value(client.create_camera(&config, &key).await?)?
                }
                Cameras::Update {
                    id,
                    config,
                    revision,
                    key,
                } => {
                    let patch: CameraPatch = read_json(&config)?;
                    serde_json::to_value(client.patch_camera(id, &patch, &key, revision).await?)?
                }
            };
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        Command::Pair {
            data_dir,
            local,
            role,
            camera,
            scope,
            json,
            proxy,
        } => {
            let request = PairingRequest {
                role: match role {
                    Role::Owner => DeviceRole::Owner,
                    Role::Operator => DeviceRole::Operator,
                    Role::Viewer => DeviceRole::Viewer,
                },
                camera_ids: camera,
                scopes: scope
                    .into_iter()
                    .map(|scope| match scope {
                        Scope::EventsAck => DeviceScope::EventsAck,
                        Scope::EvidenceRead => DeviceScope::EvidenceRead,
                        Scope::StreamsRead => DeviceScope::StreamsRead,
                    })
                    .collect(),
            };
            pair(data_dir, local, request, json, proxy).await?;
        }
        Command::BackupOffline {
            data_dir,
            destination,
        } => {
            let store = Store::open(&data_dir, StoreConfig::default())?;
            store.backup(std::path::absolute(destination)?).await?;
            println!("Backup verified for {}", store.identity().instance_id);
        }
        Command::Restore {
            backup,
            destination,
        } => {
            let identity = restore_backup(&backup, &std::path::absolute(destination)?)?;
            println!(
                "Restored {} with stream generation {}",
                identity.instance_id, identity.generation
            );
        }
        Command::Migrate {
            legacy_data,
            destination,
            config,
            dry_run,
        } => {
            let options: LegacyImport = read_json(&config)?;
            let manifest = import_legacy(
                &legacy_data,
                &std::path::absolute(destination)?,
                options,
                dry_run,
            )?;
            println!("{}", serde_json::to_string_pretty(&manifest)?);
        }
    }
    Ok(())
}

async fn pair(
    directory: PathBuf,
    local: SocketAddr,
    request: PairingRequest,
    json: bool,
    proxy: bool,
) -> Result<()> {
    ensure!(
        request.valid(),
        "invalid requested role, camera list or scopes"
    );
    let client = local_client(&directory, local)?;
    let offer = if proxy {
        client.issue_proxy_pairing(&request).await
    } else {
        client.issue_pairing(&request).await
    }
    .context("issue pairing offer")?;
    let data = serde_json::to_string(&offer)?;
    if json {
        println!("{data}");
    } else {
        let qr = qrcode::QrCode::with_error_correction_level(data.as_bytes(), qrcode::EcLevel::L)?;
        println!(
            "{}",
            qr.render::<qrcode::render::unicode::Dense1x2>().build()
        );
        eprintln!(
            "Scan the one-time offer from a trusted screen within five minutes. Use --json for manual import."
        );
    }
    Ok(())
}

fn local_client(directory: &Path, local: SocketAddr) -> Result<Client> {
    ensure!(
        local.ip().is_loopback(),
        "maintenance credentials may only be sent to the local Core"
    );
    let mut token = String::new();
    File::open(directory.join("local-access-token"))?
        .take(513)
        .read_to_string(&mut token)
        .context("read local access token")?;
    ensure!(token.len() <= 512, "invalid local access token");
    let credentials = Arc::new(StaticToken::new(token.trim().to_owned())?);
    Ok(Client::loopback(&format!("http://{local}"), credentials)?)
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    const MAX_BYTES: usize = 16 * 1024;
    let mut bytes = Vec::new();
    File::open(path)?
        .take((MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= MAX_BYTES, "configuration exceeds 16 KiB");
    serde_json::from_slice(&bytes).context("configuration does not match the JSON contract")
}
