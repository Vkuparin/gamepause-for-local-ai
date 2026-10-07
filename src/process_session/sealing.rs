//! Sealing of a captured start command with Windows per-user data protection,
//! so it is never persisted, logged or displayed in the clear.

use super::Launch;
use anyhow::{Context, Result, bail};
/// Encrypts with Windows per-user data protection and hex-encodes the result.
/// Another account or another PC cannot read it.
pub fn seal(launch: &Launch) -> Result<String> {
    let plain = serde_json::to_vec(launch)?;
    Ok(protect(&plain, true)?
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}
pub fn open(sealed: &str) -> Result<Launch> {
    if sealed.is_empty() || !sealed.len().is_multiple_of(2) || !sealed.is_ascii() {
        bail!("Saved start command is missing or malformed");
    }
    let bytes = (0..sealed.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&sealed[index..index + 2], 16))
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("Saved start command is malformed")?;
    serde_json::from_slice(&protect(&bytes, false)?).context("Saved start command is malformed")
}
fn protect(input: &[u8], seal: bool) -> Result<Vec<u8>> {
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{
            CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
        },
    };
    let source = CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(input.len()).context("Start command is too large")?,
        pbData: input.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    let ok = unsafe {
        if seal {
            CryptProtectData(
                &source,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &source,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        }
    };
    if ok == 0 || output.pbData.is_null() {
        bail!("Windows data protection refused the start command");
    }
    let bytes =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec();
    unsafe {
        LocalFree(output.pbData.cast());
    }
    Ok(bytes)
}
