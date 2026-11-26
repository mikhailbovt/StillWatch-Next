use std::{
    fs::{self, File},
    io::{ErrorKind, Read, Write},
    path::Path,
};

use ring::aead;

use crate::StoreError;

const KEY_BYTES: usize = 32;
const MAX_PROTECTED_BYTES: u64 = 4096;

pub fn load(root: &Path) -> Result<aead::LessSafeKey, StoreError> {
    let path = root.join("refresh-receipts.key");
    let mut protected = match fs::symlink_metadata(&path) {
        Ok(metadata) => {
            if !metadata.file_type().is_file() || metadata.len() > MAX_PROTECTED_BYTES {
                return Err(StoreError::Invalid);
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if metadata.permissions().mode() & 0o077 != 0 {
                    return Err(StoreError::Invalid);
                }
            }
            let mut value = Vec::new();
            File::open(&path)?
                .take(MAX_PROTECTED_BYTES + 1)
                .read_to_end(&mut value)?;
            if value.len() as u64 > MAX_PROTECTED_BYTES {
                return Err(StoreError::Invalid);
            }
            value
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {
            let mut key = [0; KEY_BYTES];
            getrandom::fill(&mut key).map_err(|_| StoreError::Unavailable)?;
            let value = protect(&key);
            key.fill(0);
            let value = value?;
            let mut file = tempfile::NamedTempFile::new_in(root)?;
            file.write_all(&value)?;
            file.as_file().sync_all()?;
            file.persist_noclobber(&path).map_err(|error| error.error)?;
            crate::evidence::sync_directory(root)?;
            value
        }
        Err(error) => return Err(error.into()),
    };
    let result = unprotect(&protected);
    protected.fill(0);
    let mut key = result?;
    if key.len() != KEY_BYTES {
        return Err(StoreError::Invalid);
    }
    let result = aead::UnboundKey::new(&aead::AES_256_GCM, &key);
    key.fill(0);
    result
        .map(aead::LessSafeKey::new)
        .map_err(|_| StoreError::Invalid)
}

#[cfg(unix)]
fn protect(key: &[u8]) -> std::io::Result<Vec<u8>> {
    // The service account owns the 0600 key file outside the database and its backups.
    Ok(key.to_vec())
}

#[cfg(unix)]
fn unprotect(key: &[u8]) -> std::io::Result<Vec<u8>> {
    Ok(key.to_vec())
}

#[cfg(windows)]
fn protect(key: &[u8]) -> std::io::Result<Vec<u8>> {
    dpapi(key, true)
}

#[cfg(windows)]
fn unprotect(key: &[u8]) -> std::io::Result<Vec<u8>> {
    dpapi(key, false)
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn dpapi(key: &[u8], encrypt: bool) -> std::io::Result<Vec<u8>> {
    use std::{io, ptr, slice};
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{
            CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
        },
    };

    if key.is_empty() || key.len() > MAX_PROTECTED_BYTES as usize {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            "invalid protected refresh key",
        ));
    }
    let mut bytes = key.to_vec();
    let input = CRYPT_INTEGER_BLOB {
        cbData: bytes.len() as u32,
        pbData: bytes.as_mut_ptr(),
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: ptr::null_mut(),
    };
    // SAFETY: the input slice and output struct remain live for both calls.
    // All unused DPAPI parameters are optional and no UI is allowed.
    let ok = unsafe {
        if encrypt {
            CryptProtectData(
                &input,
                ptr::null(),
                ptr::null(),
                ptr::null(),
                ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &input,
                ptr::null_mut(),
                ptr::null(),
                ptr::null(),
                ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        }
    };
    bytes.fill(0);
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    if output.pbData.is_null() {
        return Err(io::Error::new(ErrorKind::InvalidData, "empty DPAPI output"));
    }
    // SAFETY: successful DPAPI allocated cbData writable bytes. The copy
    // remains owned by Rust after the original buffer is cleared and freed.
    let value = unsafe {
        let data = slice::from_raw_parts_mut(output.pbData, output.cbData as usize);
        let value = (data.len() as u64 <= MAX_PROTECTED_BYTES).then(|| data.to_vec());
        data.fill(0);
        LocalFree(output.pbData.cast());
        value
    };
    value.ok_or_else(|| {
        io::Error::new(
            ErrorKind::InvalidData,
            "DPAPI output exceeds refresh key limit",
        )
    })
}
