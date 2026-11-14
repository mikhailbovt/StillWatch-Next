use std::path::Path;

use sw_protocol::worker::CameraCredentials;
use tokio::io::AsyncReadExt;
use uuid::Uuid;

use crate::RegistryError;

pub(crate) const MAX_CLEAR_BYTES: usize = 4096;
#[cfg(windows)]
const MAX_ENCRYPTED_BYTES: usize = 16 * 1024;

pub(crate) async fn read(root: &Path, id: Uuid) -> Result<CameraCredentials, RegistryError> {
    #[cfg(windows)]
    let protected = read_file(&root.join(format!("{id}.dpapi")), MAX_ENCRYPTED_BYTES)
        .await
        .map_err(|_| RegistryError::Source)?;
    #[cfg(windows)]
    let clear = match protected {
        Some(mut payload) => {
            tokio::task::spawn_blocking(move || crate::windows_credentials::decrypt(&mut payload))
                .await
                .map_err(|_| RegistryError::Source)?
                .map_err(|_| RegistryError::Source)?
        }
        // Existing mounted credentials still work until the administrator rotates them.
        None => read_file(&root.join(format!("{id}.json")), MAX_CLEAR_BYTES)
            .await
            .map_err(|_| RegistryError::Source)?
            .ok_or(RegistryError::Source)?,
    };
    #[cfg(not(windows))]
    let clear = read_file(&root.join(format!("{id}.json")), MAX_CLEAR_BYTES)
        .await
        .map_err(|_| RegistryError::Source)?
        .ok_or(RegistryError::Source)?;

    let credentials: CameraCredentials =
        serde_json::from_slice(&clear).map_err(|_| RegistryError::Source)?;
    if credentials.username.is_empty()
        || credentials.username.len() > 512
        || credentials.password.len() > 1024
        || credentials.username.chars().any(char::is_control)
        || credentials.password.chars().any(char::is_control)
    {
        return Err(RegistryError::Source);
    }
    Ok(credentials)
}

async fn read_file(path: &Path, limit: usize) -> std::io::Result<Option<Vec<u8>>> {
    use std::io::ErrorKind;

    let meta = match tokio::fs::symlink_metadata(path).await {
        Ok(meta) => meta,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !meta.file_type().is_file() || meta.len() > limit as u64 {
        return Err(ErrorKind::InvalidData.into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(ErrorKind::PermissionDenied.into());
        }
    }
    let mut data = Vec::new();
    tokio::fs::File::open(path)
        .await?
        .take(limit as u64 + 1)
        .read_to_end(&mut data)
        .await?;
    if data.len() > limit {
        return Err(ErrorKind::InvalidData.into());
    }
    Ok(Some(data))
}
