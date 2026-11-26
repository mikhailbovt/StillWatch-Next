use std::{io, ptr, slice};

use windows_sys::Win32::{
    Foundation::LocalFree,
    Security::Cryptography::{CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptUnprotectData},
};

use crate::camera_credentials::MAX_CLEAR_BYTES;

pub(crate) fn decrypt(encrypted: &mut [u8]) -> io::Result<Vec<u8>> {
    let input = CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(encrypted.len()).map_err(io::Error::other)?,
        pbData: encrypted.as_mut_ptr(),
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: ptr::null_mut(),
    };
    // SAFETY: The input slice is valid for this call. The out parameter starts
    // empty and receives a LocalAlloc buffer on success. UI and optional input
    // entropy are disabled for service-account reads.
    let ok = unsafe {
        CryptUnprotectData(
            &input,
            ptr::null_mut(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    if output.pbData.is_null() {
        return Err(io::ErrorKind::InvalidData.into());
    }
    // SAFETY: DPAPI supplies cbData bytes on success. Free that buffer exactly
    // once after any bounded copy has finished.
    let data = unsafe {
        let bytes = output.cbData as usize;
        let data = (bytes <= MAX_CLEAR_BYTES)
            .then(|| slice::from_raw_parts(output.pbData, bytes).to_vec());
        LocalFree(output.pbData.cast());
        data
    };
    data.ok_or_else(|| io::ErrorKind::InvalidData.into())
}
