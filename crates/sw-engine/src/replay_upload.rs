use std::{
    fs,
    path::{Path, PathBuf},
};

use futures_util::{Stream, StreamExt};
use sw_protocol::{
    Counter, MAX_REPLAY_UPLOAD_BYTES, ReplayUpload,
    worker::{CaptureRequest, CaptureSource},
};
use tempfile::TempPath;
use tokio::{io::AsyncWriteExt, sync::Semaphore};
use uuid::Uuid;

use crate::{CameraRegistry, RegistryError};

const DIRECTORY: &str = ".stillwatch-uploads";
const TEMPORARY: &str = ".partial-";
const QUOTA: u64 = 2 * 1024 * 1024 * 1024;

pub(crate) struct ReplayUploads {
    root: PathBuf,
    slot: Semaphore,
}

impl ReplayUploads {
    pub fn open(replay_root: &Path) -> Result<Self, RegistryError> {
        let root = replay_root.join(DIRECTORY);
        // Keep ordinary host-provisioned replay available on read-only mounts.
        if private_directory(&root) {
            for entry in fs::read_dir(&root).map_err(RegistryError::UploadStorage)? {
                let entry = entry.map_err(RegistryError::UploadStorage)?;
                if entry.file_name().to_string_lossy().starts_with(TEMPORARY)
                    && entry
                        .file_type()
                        .map_err(RegistryError::UploadStorage)?
                        .is_file()
                {
                    // An administrator can mount an existing replay directory read-only.
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
        Ok(Self {
            root,
            slot: Semaphore::new(1),
        })
    }

    pub fn permits_source(&self, path: &Path) -> bool {
        let Ok(path) = path.strip_prefix(DIRECTORY) else {
            return true;
        };
        path.parent() == Some(Path::new(""))
            && path.extension().is_some_and(|ext| ext == "video")
            && path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .and_then(|name| name.parse::<Uuid>().ok())
                .is_some()
    }
}

impl CameraRegistry {
    pub async fn upload_replay<B: AsRef<[u8]>, E>(
        &self,
        input: impl Stream<Item = Result<B, E>>,
        bytes: u64,
    ) -> Result<ReplayUpload, RegistryError> {
        if bytes == 0 || bytes > MAX_REPLAY_UPLOAD_BYTES {
            return Err(RegistryError::Invalid);
        }
        let _slot = self
            .uploads
            .slot
            .try_acquire()
            .map_err(|_| RegistryError::Busy)?;
        let root = self.uploads.root.clone();
        let (file, path) = tokio::task::spawn_blocking(move || prepare(&root, bytes))
            .await
            .map_err(|error| RegistryError::UploadStorage(std::io::Error::other(error)))??;
        let mut output = tokio::fs::File::from_std(file);
        tokio::pin!(input);
        let mut remaining = bytes;
        while let Some(chunk) = input.next().await {
            let chunk = chunk.map_err(|_| RegistryError::Invalid)?;
            let bytes = chunk.as_ref();
            remaining = remaining
                .checked_sub(bytes.len() as u64)
                .ok_or(RegistryError::Invalid)?;
            output
                .write_all(bytes)
                .await
                .map_err(RegistryError::UploadStorage)?;
        }
        if remaining != 0 {
            return Err(RegistryError::Invalid);
        }
        output
            .sync_all()
            .await
            .map_err(RegistryError::UploadStorage)?;
        drop(output);

        let request = CaptureRequest {
            source: CaptureSource::Replay {
                file: path.to_path_buf(),
                pace: false,
            },
            masks: vec![],
        };
        let (source_probe, sample) = self.probe_request(request, false).await.map_err(|error| {
            if matches!(
                error,
                RegistryError::Worker(crate::WorkerError::Exit | crate::WorkerError::Deadline)
            ) {
                RegistryError::Invalid
            } else {
                error
            }
        })?;
        if sample.is_none() {
            return Err(RegistryError::Busy);
        }
        let name = format!("{}.video", Uuid::new_v4());
        let destination = self.uploads.root.join(&name);
        let file = format!("{DIRECTORY}/{name}");
        tokio::task::spawn_blocking(move || publish(path, &destination))
            .await
            .map_err(|error| RegistryError::UploadStorage(std::io::Error::other(error)))??;
        Ok(ReplayUpload {
            file,
            bytes: Counter(bytes),
            source_probe,
        })
    }
}

fn prepare(root: &Path, incoming: u64) -> Result<(fs::File, TempPath), RegistryError> {
    #[cfg(unix)]
    let directory = {
        use std::os::unix::fs::DirBuilderExt;
        let mut directory = fs::DirBuilder::new();
        directory.mode(0o700);
        directory
    };
    #[cfg(windows)]
    let directory = fs::DirBuilder::new();
    match directory.create(root) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(RegistryError::UploadStorage(error)),
    }
    if !private_directory(root) {
        return Err(RegistryError::Source);
    }
    let mut used = 0u64;
    for entry in fs::read_dir(root).map_err(RegistryError::UploadStorage)? {
        let entry = entry.map_err(RegistryError::UploadStorage)?;
        if entry
            .file_type()
            .map_err(RegistryError::UploadStorage)?
            .is_file()
        {
            used = used.saturating_add(
                entry
                    .metadata()
                    .map_err(RegistryError::UploadStorage)?
                    .len(),
            );
            if used.saturating_add(incoming) > QUOTA {
                return Err(RegistryError::UploadQuota);
            }
        }
    }
    tempfile::Builder::new()
        .prefix(TEMPORARY)
        .tempfile_in(root)
        .map(tempfile::NamedTempFile::into_parts)
        .map_err(RegistryError::UploadStorage)
}

fn private_directory(path: &Path) -> bool {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return false;
    };
    if !meta.is_dir() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o077 == 0
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // Reject every reparse point, including directory junctions.
        meta.file_attributes() & 0x400 == 0
    }
}

fn publish(path: TempPath, destination: &Path) -> Result<(), RegistryError> {
    path.persist_noclobber(destination)
        .map_err(|error| RegistryError::UploadStorage(error.error))?;
    #[cfg(unix)]
    {
        let directory = destination.parent().ok_or(RegistryError::Source)?;
        fs::File::open(directory)
            .and_then(|file| file.sync_all())
            .map_err(RegistryError::UploadStorage)?;
    }
    Ok(())
}
