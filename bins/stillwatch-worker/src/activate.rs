use std::{io::Write, path::Path};

use anyhow::{Context, Result, ensure};
use sw_protocol::worker::{MODEL_SELECTION_MAX_BYTES, ModelSelection};
use tempfile::NamedTempFile;

pub fn publish(selection: &Path, model: ModelSelection) -> Result<()> {
    let selection = std::path::absolute(selection)?;
    let directory = selection
        .parent()
        .context("model selection needs a file name")?;
    let bytes = serde_json::to_vec_pretty(&model)?;
    ensure!(
        bytes.len() < MODEL_SELECTION_MAX_BYTES,
        "model selection exceeds the size limit"
    );
    if let Ok(metadata) = selection.symlink_metadata() {
        ensure!(metadata.is_file(), "model selection must be a regular file");
    }

    // Exercise every model and reference fixture on the selected provider.
    crate::parity::verify(
        &model.library,
        &model.library_sha256,
        &model.models,
        &model.manifest_sha256,
        model.cuda,
    )?;

    let mut staged = NamedTempFile::new_in(directory)?;
    #[cfg(unix)]
    {
        use std::{fs::Permissions, os::unix::fs::PermissionsExt};
        // The protected directory governs access. The file contains paths and
        // public digests and must be readable by the unprivileged service.
        staged
            .as_file()
            .set_permissions(Permissions::from_mode(0o644))?;
    }
    staged.write_all(&bytes)?;
    staged.write_all(b"\n")?;
    staged.as_file().sync_all()?;
    staged
        .persist(&selection)
        .map_err(|error| error.error)
        .context("publish verified model selection")?;
    #[cfg(unix)]
    std::fs::File::open(directory)?.sync_all()?;
    Ok(())
}
