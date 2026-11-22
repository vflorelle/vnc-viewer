//! Mots de passe mémorisés dans la configuration.
//!
//! Sous Windows, chiffrés par DPAPI : seul le même compte Windows, sur le même PC, peut les
//! relire (`dpapi:` + hexadécimal). Ailleurs, seulement masqués (`hex:`), le fichier restant
//! lisible par son seul propriétaire.

pub fn protect(password: &str) -> String {
    if password.is_empty() {
        return String::new();
    }
    #[cfg(windows)]
    if let Some(data) = dpapi::protect(password.as_bytes()) {
        return format!("dpapi:{}", hex(&data));
    }
    format!("hex:{}", hex(password.as_bytes()))
}

/// `None` si le mot de passe ne peut pas être relu (autre PC, autre compte…).
pub fn unprotect(stored: &str) -> Option<String> {
    if let Some(data) = stored.strip_prefix("hex:") {
        return String::from_utf8(unhex(data)?).ok();
    }
    #[cfg(windows)]
    if let Some(data) = stored.strip_prefix("dpapi:") {
        return String::from_utf8(dpapi::unprotect(&unhex(data)?)?).ok();
    }
    None
}

fn hex(data: &[u8]) -> String {
    data.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

#[cfg(windows)]
mod dpapi {
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
    };

    pub fn protect(data: &[u8]) -> Option<Vec<u8>> {
        run(data, true)
    }

    pub fn unprotect(data: &[u8]) -> Option<Vec<u8>> {
        run(data, false)
    }

    fn run(data: &[u8], protect: bool) -> Option<Vec<u8>> {
        let input = CRYPT_INTEGER_BLOB {
            cbData: u32::try_from(data.len()).ok()?,
            pbData: data.as_ptr().cast_mut(),
        };
        let mut output = CRYPT_INTEGER_BLOB::default();
        // SAFETY : `input` pointe sur `data`, vivant pendant l'appel ; `output` est alloué par
        // Windows, copié puis libéré par LocalFree.
        unsafe {
            if protect {
                CryptProtectData(&input, None, None, None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut output).ok()?;
            } else {
                CryptUnprotectData(&input, None, None, None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut output).ok()?;
            }
            let result = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
            LocalFree(Some(HLOCAL(output.pbData.cast())));
            Some(result)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        for password in ["", "secret", "àé€ 😀"] {
            assert_eq!(unprotect(&protect(password)).unwrap_or_default(), password);
        }
        assert_eq!(unprotect("hex:6162"), Some("ab".into()));
        assert_eq!(unprotect("hex:6"), None);
        assert_eq!(unprotect("inconnu"), None);
    }
}
