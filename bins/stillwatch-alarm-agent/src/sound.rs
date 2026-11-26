#[cfg(target_os = "linux")]
use anyhow::Context;
use anyhow::{Result, ensure};
use stillwatch_sdk::protocol::AlertDelivery;

use crate::transport::{NotificationTransport, OutputUnavailable};

pub struct Output {
    wave: Vec<u8>,
    #[cfg(target_os = "linux")]
    player: &'static str,
}

impl NotificationTransport for Output {
    async fn present(&self, alert: &AlertDelivery) -> Result<(), OutputUnavailable> {
        eprintln!("StillWatch urgent incident {}", alert.incident_id);
        self.repeat(alert).await
    }

    async fn repeat(&self, _: &AlertDelivery) -> Result<(), OutputUnavailable> {
        self.play().await.map_err(|_| OutputUnavailable("audio"))
    }
}

impl Output {
    pub fn new() -> Result<Self> {
        let wave = tone();
        #[cfg(windows)]
        return Ok(Self { wave });
        #[cfg(target_os = "linux")]
        {
            let player = ["/usr/bin/paplay", "/usr/bin/aplay"]
                .into_iter()
                .find(|path| std::path::Path::new(path).is_file())
                .context("install a PulseAudio or ALSA player for the local alarm")?;
            Ok(Self { wave, player })
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        anyhow::bail!("local alarm audio is available on Windows and Linux")
    }

    #[cfg(windows)]
    pub async fn play(&self) -> Result<()> {
        use windows_sys::Win32::Media::Audio::{PlaySoundW, SND_MEMORY, SND_NODEFAULT, SND_SYNC};

        let wave = self.wave.clone();
        tokio::task::spawn_blocking(move || {
            // SAFETY: SND_MEMORY interprets the pointer as a WAVE file.
            // Synchronous playback ends before the complete buffer is dropped.
            let result = unsafe {
                PlaySoundW(
                    wave.as_ptr().cast(),
                    std::ptr::null_mut(),
                    SND_MEMORY | SND_NODEFAULT | SND_SYNC,
                )
            };
            ensure!(result != 0, "Windows audio output is unavailable");
            Ok(())
        })
        .await?
    }

    #[cfg(target_os = "linux")]
    pub async fn play(&self) -> Result<()> {
        use std::process::Stdio;
        use tokio::io::AsyncWriteExt;

        let mut command = tokio::process::Command::new(self.player);
        if self.player == "/usr/bin/paplay" {
            command.arg("--latency-msec=100");
        }
        let mut child = command
            .kill_on_drop(true)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let result = tokio::time::timeout(std::time::Duration::from_secs(3), async {
            if let Some(mut input) = child.stdin.take() {
                input.write_all(&self.wave).await?;
            }
            child.wait().await
        })
        .await;
        match result {
            Ok(status) => ensure!(status?.success(), "Linux audio output is unavailable"),
            Err(_) => {
                let _ = child.kill().await;
                anyhow::bail!("Linux audio playback timed out");
            }
        }
        Ok(())
    }

    #[cfg(not(any(windows, target_os = "linux")))]
    pub async fn play(&self) -> Result<()> {
        anyhow::bail!("local alarm audio is available on Windows and Linux")
    }
}

fn tone() -> Vec<u8> {
    const RATE: u32 = 16_000;
    const SAMPLES: u32 = RATE * 3 / 5;
    let mut data = Vec::with_capacity((44 + SAMPLES * 2) as usize);
    data.extend_from_slice(b"RIFF");
    data.extend_from_slice(&(36 + SAMPLES * 2).to_le_bytes());
    data.extend_from_slice(b"WAVEfmt ");
    data.extend_from_slice(&16u32.to_le_bytes());
    data.extend_from_slice(&1u16.to_le_bytes());
    data.extend_from_slice(&1u16.to_le_bytes());
    data.extend_from_slice(&RATE.to_le_bytes());
    data.extend_from_slice(&(RATE * 2).to_le_bytes());
    data.extend_from_slice(&2u16.to_le_bytes());
    data.extend_from_slice(&16u16.to_le_bytes());
    data.extend_from_slice(b"data");
    data.extend_from_slice(&(SAMPLES * 2).to_le_bytes());
    for sample in 0..SAMPLES {
        let seconds = f64::from(sample) / f64::from(RATE);
        let pulse = seconds % 0.3;
        let envelope = (pulse / 0.018).min(1.0) * ((0.22 - pulse) / 0.035).clamp(0.0, 1.0);
        let sine =
            (std::f64::consts::TAU * if seconds < 0.3 { 880.0 } else { 660.0 } * seconds).sin();
        data.extend_from_slice(&((sine * envelope * 10_000.0) as i16).to_le_bytes());
    }
    data
}
