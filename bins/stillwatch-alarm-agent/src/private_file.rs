use std::{fs, io::Read, path::Path};

#[cfg(windows)]
use anyhow::Context;
use anyhow::{Result, ensure};

const MAX_BYTES: u64 = 32 * 1024;

pub fn read(path: &Path) -> Result<Vec<u8>> {
    let meta = fs::symlink_metadata(path)?;
    ensure!(
        meta.file_type().is_file() && meta.len() <= MAX_BYTES,
        "alarm agent state is invalid"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            meta.permissions().mode() & 0o077 == 0,
            "alarm agent state must be private"
        );
    }
    let mut data = Vec::new();
    fs::File::open(path)?
        .take(MAX_BYTES + 1)
        .read_to_end(&mut data)?;
    ensure!(
        data.len() as u64 <= MAX_BYTES,
        "alarm agent state is invalid"
    );
    Ok(data)
}

#[cfg(unix)]
pub fn encrypt(plain: &[u8]) -> Result<Vec<u8>> {
    ensure!(
        plain.len() as u64 <= MAX_BYTES,
        "alarm agent state is too large"
    );
    Ok(plain.to_vec())
}

#[cfg(unix)]
pub fn decrypt(bytes: &[u8]) -> Result<Vec<u8>> {
    ensure!(
        bytes.len() as u64 <= MAX_BYTES,
        "alarm agent state is too large"
    );
    Ok(bytes.to_vec())
}

#[cfg(windows)]
pub fn encrypt(bytes: &[u8]) -> Result<Vec<u8>> {
    dpapi(bytes, true).context("protect alarm credentials with DPAPI")
}

#[cfg(windows)]
pub fn decrypt(bytes: &[u8]) -> Result<Vec<u8>> {
    dpapi(bytes, false).context("unprotect alarm credentials with DPAPI")
}

#[cfg(windows)]
fn dpapi(bytes: &[u8], encrypt: bool) -> Result<Vec<u8>> {
    use std::{ptr, slice};
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{
            CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
        },
    };

    ensure!(
        bytes.len() as u64 <= MAX_BYTES && !bytes.is_empty(),
        "invalid DPAPI input"
    );
    let mut input = bytes.to_vec();
    let input = CRYPT_INTEGER_BLOB {
        cbData: input.len().try_into()?,
        pbData: input.as_mut_ptr(),
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: ptr::null_mut(),
    };
    // SAFETY: input owns a valid byte buffer for the call. DPAPI writes an
    // allocated blob into output and all optional parameters may be null.
    let status = unsafe {
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
    if status == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    ensure!(!output.pbData.is_null(), "DPAPI output is empty");
    // SAFETY: DPAPI documents a LocalAlloc buffer of cbData bytes on success.
    // Copy its contents before relinquishing ownership to LocalFree.
    let result = unsafe {
        let result = slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        LocalFree(output.pbData.cast());
        result
    };
    ensure!(
        result.len() as u64 <= MAX_BYTES,
        "DPAPI output exceeds the limit"
    );
    Ok(result)
}
