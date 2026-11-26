use std::{
    fs::{self, File},
    io::{Read, Write},
    path::Path,
};

use anyhow::{Result, anyhow, ensure};
use sha2::{Digest, Sha256};

pub fn load_or_create(directory: &Path) -> Result<[u8; 32]> {
    let path = directory.join("local-access-token");
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut secret = [0u8; 32];
            getrandom::fill(&mut secret)
                .map_err(|_| anyhow!("operating system randomness unavailable"))?;
            let mut staged = tempfile::NamedTempFile::new_in(directory)?;
            for byte in secret {
                write!(staged, "{byte:02x}")?;
            }
            writeln!(staged)?;
            staged.as_file().sync_all()?;
            staged
                .persist_noclobber(&path)
                .map_err(|error| error.error)?;
        }
        Err(error) => return Err(error.into()),
        Ok(_) => {}
    }
    read_digest(&path)
}

pub fn read_digest(path: &Path) -> Result<[u8; 32]> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.file_type().is_file() && metadata.len() == 65,
        "invalid token file"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            metadata.permissions().mode() & 0o077 == 0,
            "token permissions must restrict access to its owner"
        );
    }
    let mut secret = [0u8; 65];
    File::open(path)?.read_exact(&mut secret)?;
    let (token, suffix) = secret.split_at(64);
    ensure!(
        suffix == b"\n" && token.iter().all(u8::is_ascii_hexdigit),
        "invalid token file"
    );
    Ok(digest(token))
}

pub fn digest(token: &[u8]) -> [u8; 32] {
    Sha256::digest(token).into()
}
