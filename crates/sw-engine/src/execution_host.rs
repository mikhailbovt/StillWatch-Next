use std::{io, path::PathBuf, process::Stdio, time::Duration};

use sw_protocol::{ExecutionHost, HostAccelerator};
use tokio::io::AsyncReadExt;

use crate::capture_process::reap;
use crate::child_process::command;

pub async fn execution_host(cuda_device: Option<u16>) -> io::Result<ExecutionHost> {
    let (cpu, ram_bytes, os_release) = platform()?;
    let accelerator = match cuda_device {
        Some(device) => Some(nvidia(device).await?),
        None => None,
    };
    Ok(ExecutionHost {
        platform: std::env::consts::OS.into(),
        architecture: std::env::consts::ARCH.into(),
        cpu,
        ram_bytes,
        os_release,
        logical_cores: std::thread::available_parallelism()?.get() as u32,
        accelerator,
    })
}

#[cfg(target_os = "linux")]
fn platform() -> io::Result<(String, u64, String)> {
    let cpuinfo = std::fs::read_to_string("/proc/cpuinfo")?;
    let field = |key| {
        cpuinfo
            .lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.trim() == key)
            .map(|(_, value)| value.trim())
    };
    let cpu = ["vendor_id", "model name", "cpu family", "model", "stepping"]
        .map(|key| field(key).ok_or_else(unavailable))
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?
        .join(" / ");
    let meminfo = std::fs::read_to_string("/proc/meminfo")?;
    let memory = meminfo
        .lines()
        .find_map(|line| line.strip_prefix("MemTotal:"))
        .and_then(|value| value.split_whitespace().next())
        .and_then(|value| value.parse::<u64>().ok())
        .and_then(|kib| kib.checked_mul(1024))
        .ok_or_else(unavailable)?;
    let os_release = std::fs::read_to_string("/proc/sys/kernel/osrelease")?;
    Ok((cpu, memory, os_release.trim().into()))
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn platform() -> io::Result<(String, u64, String)> {
    use windows_sys::Win32::System::{
        Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW},
        SystemInformation::GetPhysicallyInstalledSystemMemory,
    };

    fn value<T>(key: &str, name: &str, flags: u32, output: &mut [T]) -> io::Result<()> {
        let wide = |text: &str| text.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
        let key = wide(key);
        let name = wide(name);
        let mut length = std::mem::size_of_val(output) as u32;
        // SAFETY: the key and name are NUL-terminated. The destination has `length`
        // writable bytes and stays live for the synchronous call.
        let status = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                key.as_ptr(),
                name.as_ptr(),
                flags,
                std::ptr::null_mut(),
                output.as_mut_ptr().cast(),
                &mut length,
            )
        };
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        Ok(())
    }
    fn string(key: &str, name: &str) -> io::Result<String> {
        let mut buffer = [0u16; 256];
        value(key, name, RRF_RT_REG_SZ, &mut buffer)?;
        let length = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
        String::from_utf16(&buffer[..length]).map_err(|_| unavailable())
    }
    let processor = r"HARDWARE\DESCRIPTION\System\CentralProcessor\0";
    let version = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
    let cpu = ["VendorIdentifier", "ProcessorNameString", "Identifier"]
        .map(|name| string(processor, name))
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?
        .join(" / ");
    let build = string(version, "CurrentBuildNumber")?;
    let mut revision = [0u32];
    value(version, "UBR", RRF_RT_REG_DWORD, &mut revision)?;
    let mut kib = 0u64;
    // SAFETY: the API initializes this correctly sized output on success.
    if unsafe { GetPhysicallyInstalledSystemMemory(&mut kib) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((
        cpu,
        kib.checked_mul(1024).ok_or_else(unavailable)?,
        format!("{build}.{}", revision[0]),
    ))
}

#[cfg(not(any(target_os = "linux", windows)))]
fn platform() -> io::Result<(String, u64, String)> {
    Err(unavailable())
}

async fn nvidia(device: u16) -> io::Result<HostAccelerator> {
    let driver = if cfg!(windows) {
        PathBuf::from(std::env::var_os("SystemRoot").ok_or_else(unavailable)?)
            .join("System32/nvidia-smi.exe")
    } else {
        ["/usr/bin/nvidia-smi", "/usr/lib/wsl/lib/nvidia-smi"]
            .into_iter()
            .map(PathBuf::from)
            .find(|path| path.is_file())
            .ok_or_else(unavailable)?
    };
    let mut child = command(&driver, |command| {
        command
            .arg(format!("--id={device}"))
            .args([
                "--query-gpu=name,compute_cap,driver_version,memory.total,pci.device_id",
                "--format=csv,noheader,nounits",
            ])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .stdout(Stdio::piped());
    })
    .spawn()?;
    let stdout = child.stdout().take().ok_or_else(unavailable)?;
    let mut bytes = Vec::with_capacity(1024);
    let probe = tokio::time::timeout(Duration::from_secs(3), async {
        stdout.take(1025).read_to_end(&mut bytes).await?;
        if bytes.len() > 1024 || !child.wait().await?.success() {
            return Err(unavailable());
        }
        Ok(())
    })
    .await;
    if !matches!(probe, Ok(Ok(()))) {
        let _ = reap(&mut *child).await;
        return Err(unavailable());
    }
    let output = std::str::from_utf8(&bytes)
        .map_err(|_| unavailable())?
        .trim();
    let fields: Vec<_> = output.split(',').map(str::trim).collect();
    match fields.as_slice() {
        [name, architecture, driver, memory, device_id] if !output.contains(['\r', '\n']) => {
            Ok(HostAccelerator {
                name: (*name).into(),
                architecture: (*architecture).into(),
                driver: (*driver).into(),
                memory_mib: memory.parse().map_err(|_| unavailable())?,
                device_id: (*device_id).into(),
            })
        }
        _ => Err(unavailable()),
    }
}

fn unavailable() -> io::Error {
    io::Error::other("Execution host fingerprint unavailable")
}
