//! Protocole RFB (VNC) côté client : adresse, version, sécurité, initialisation, messages envoyés
//! au serveur.

use std::fmt;
use std::io::{self, Read, Write};

use crate::config::Encryption;
use crate::security;

pub const DEFAULT_PORT: u16 = 5900;

/// Adresse d'un serveur VNC.
#[derive(Clone, Debug, PartialEq)]
pub struct Address {
    pub host: String,
    pub port: u16,
}

impl Address {
    pub fn eq_ignore_case(&self, other: &Address) -> bool {
        self.port == other.port && self.host.eq_ignore_ascii_case(&other.host)
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        match self.port {
            DEFAULT_PORT => write!(f, "{host}"),
            port => write!(f, "{host}::{port}"),
        }
    }
}

/// Syntaxe habituelle des visionneuses : `hôte` (port 5900), `hôte:écran` (5900 + écran, un
/// nombre de 100 ou plus étant pris comme un port), `hôte::port` ; IPv6 entre crochets.
pub fn parse_address(text: &str) -> Option<Address> {
    let text = text.trim();
    let (host, rest) = if let Some(inner) = text.strip_prefix('[') {
        let (host, rest) = inner.split_once(']')?;
        (host, rest)
    } else if text.matches(':').count() > 2 {
        // IPv6 sans crochets : pas de port possible.
        (text, "")
    } else if let Some(i) = text.find(':') {
        // `:écran` ou `::port`
        (&text[..i], &text[i..])
    } else {
        (text, "")
    };
    if host.is_empty() || host.contains(char::is_whitespace) {
        return None;
    }
    let port = if let Some(port) = rest.strip_prefix("::") {
        port.parse().ok()?
    } else if let Some(display) = rest.strip_prefix(':') {
        let n: u16 = display.parse().ok()?;
        if n < 100 { DEFAULT_PORT + n } else { n }
    } else if rest.is_empty() {
        DEFAULT_PORT
    } else {
        return None;
    };
    Some(Address {
        host: host.to_owned(),
        port,
    })
}

/// Format des pixels envoyés par le serveur.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PixelFormat {
    pub bits_per_pixel: u8,
    pub depth: u8,
    pub big_endian: bool,
    pub true_colour: bool,
    pub red_max: u16,
    pub green_max: u16,
    pub blue_max: u16,
    pub red_shift: u8,
    pub green_shift: u8,
    pub blue_shift: u8,
}

impl PixelFormat {
    /// 16 millions de couleurs.
    pub const RGB888: PixelFormat = PixelFormat::true_colour(32, 24, [255, 255, 255], [16, 8, 0]);
    /// 65 536 couleurs.
    pub const RGB565: PixelFormat = PixelFormat::true_colour(16, 16, [31, 63, 31], [11, 5, 0]);
    /// 256 couleurs.
    pub const BGR233: PixelFormat = PixelFormat::true_colour(8, 8, [7, 7, 3], [0, 3, 6]);

    const fn true_colour(bits_per_pixel: u8, depth: u8, max: [u16; 3], shift: [u8; 3]) -> Self {
        PixelFormat {
            bits_per_pixel,
            depth,
            big_endian: false,
            true_colour: true,
            red_max: max[0],
            green_max: max[1],
            blue_max: max[2],
            red_shift: shift[0],
            green_shift: shift[1],
            blue_shift: shift[2],
        }
    }

    pub fn bytes_per_pixel(&self) -> usize {
        usize::from(self.bits_per_pixel.div_ceil(8))
    }

    #[cfg(test)]
    pub fn from_bytes(b: &[u8; 16]) -> Self {
        PixelFormat {
            bits_per_pixel: b[0],
            depth: b[1],
            big_endian: b[2] != 0,
            true_colour: b[3] != 0,
            red_max: u16::from_be_bytes([b[4], b[5]]),
            green_max: u16::from_be_bytes([b[6], b[7]]),
            blue_max: u16::from_be_bytes([b[8], b[9]]),
            red_shift: b[10],
            green_shift: b[11],
            blue_shift: b[12],
        }
    }

    pub fn to_bytes(self) -> [u8; 16] {
        let mut b = [0; 16];
        b[0] = self.bits_per_pixel;
        b[1] = self.depth;
        b[2] = u8::from(self.big_endian);
        b[3] = u8::from(self.true_colour);
        b[4..6].copy_from_slice(&self.red_max.to_be_bytes());
        b[6..8].copy_from_slice(&self.green_max.to_be_bytes());
        b[8..10].copy_from_slice(&self.blue_max.to_be_bytes());
        b[10] = self.red_shift;
        b[11] = self.green_shift;
        b[12] = self.blue_shift;
        b
    }
}

/// Lecture des entiers du protocole (gros-boutistes).
pub trait ReadExt: Read {
    fn u8(&mut self) -> io::Result<u8> {
        let mut b = [0; 1];
        self.read_exact(&mut b)?;
        Ok(b[0])
    }

    fn u16(&mut self) -> io::Result<u16> {
        let mut b = [0; 2];
        self.read_exact(&mut b)?;
        Ok(u16::from_be_bytes(b))
    }

    fn u32(&mut self) -> io::Result<u32> {
        let mut b = [0; 4];
        self.read_exact(&mut b)?;
        Ok(u32::from_be_bytes(b))
    }

    fn i32(&mut self) -> io::Result<i32> {
        let mut b = [0; 4];
        self.read_exact(&mut b)?;
        Ok(i32::from_be_bytes(b))
    }

    fn read_vec(&mut self, len: usize) -> io::Result<Vec<u8>> {
        let mut data = vec![0; len];
        self.read_exact(&mut data)?;
        Ok(data)
    }

    fn skip(&mut self, len: usize) -> io::Result<()> {
        io::copy(&mut self.take(len as u64), &mut io::sink()).and_then(|n| {
            if n == len as u64 {
                Ok(())
            } else {
                Err(io::ErrorKind::UnexpectedEof.into())
            }
        })
    }
}

impl<R: Read + ?Sized> ReadExt for R {}

pub fn protocol_error(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// Connexion au serveur : flux entrant et sortant (en clair ou chiffrés).
pub struct Conn {
    pub input: Box<dyn Read + Send>,
    pub output: Box<dyn Write + Send>,
}

/// Ce que la négociation demande à l'utilisateur.
pub trait Auth {
    /// Nom d'utilisateur (si `username`) et mot de passe ; `None` : abandon.
    fn credentials(&mut self, username: bool) -> Option<(String, String)>;
    /// Accepte (ou non) la clé du serveur, d'après son empreinte.
    fn verify(&mut self, fingerprint: &str) -> bool;
}

/// Erreurs de la négociation que l'interface distingue des pannes réseau.
pub const AUTH_FAILED: io::ErrorKind = io::ErrorKind::PermissionDenied;
pub const CANCELLED: io::ErrorKind = io::ErrorKind::Interrupted;

/// Types de sécurité gérés.
pub const SEC_NONE: u8 = 1;
pub const SEC_VNC: u8 = 2;
pub const SEC_RA2: u8 = 5;
pub const SEC_RA2NE: u8 = 6;
pub const SEC_ARD: u8 = 30;
pub const SEC_RA2_256: u8 = 129;
pub const SEC_RA2NE_256: u8 = 130;

/// Ordre de préférence selon le réglage de chiffrement.
fn preference(encryption: Encryption) -> &'static [u8] {
    match encryption {
        Encryption::AlwaysOn => &[SEC_RA2_256, SEC_RA2],
        Encryption::PreferOn => &[
            SEC_RA2_256,
            SEC_RA2,
            SEC_RA2NE_256,
            SEC_RA2NE,
            SEC_ARD,
            SEC_VNC,
            SEC_NONE,
        ],
        Encryption::PreferOff => &[
            SEC_RA2NE_256,
            SEC_RA2NE,
            SEC_ARD,
            SEC_VNC,
            SEC_NONE,
            SEC_RA2_256,
            SEC_RA2,
        ],
    }
}

/// Sécurité retenue, pour l'affichage.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SecurityInfo {
    pub kind: u8,
    /// Toute la session est chiffrée (pas seulement l'authentification).
    pub encrypted: bool,
}

impl SecurityInfo {
    pub fn describe(&self) -> String {
        match self.kind {
            SEC_RA2_256 => tr!("Chiffré (RSA, AES 256 bits)", "Encrypted (RSA, 256-bit AES)"),
            SEC_RA2 => tr!("Chiffré (RSA, AES 128 bits)", "Encrypted (RSA, 128-bit AES)"),
            SEC_RA2NE | SEC_RA2NE_256 => tr!(
                "Non chiffré (mot de passe protégé par RSA-AES)",
                "Not encrypted (password protected by RSA-AES)"
            ),
            SEC_ARD => tr!(
                "Non chiffré (identifiants protégés, Apple)",
                "Not encrypted (credentials protected, Apple)"
            ),
            SEC_VNC => tr!("Non chiffré (mot de passe VNC)", "Not encrypted (VNC password)"),
            _ => tr!("Non chiffré, sans mot de passe", "Not encrypted, no password"),
        }
    }
}

/// Description du serveur après l'initialisation.
pub struct ServerInit {
    pub width: u16,
    pub height: u16,
    pub name: String,
    pub security: SecurityInfo,
    /// Version du protocole : celle annoncée par le serveur, et celle retenue (3.3, 3.7 ou 3.8).
    pub server_version: String,
    pub version: String,
}

/// Version, sécurité, authentification et initialisation.
pub fn handshake(conn: &mut Conn, encryption: Encryption, shared: bool, auth: &mut dyn Auth) -> io::Result<ServerInit> {
    let mut version = [0; 12];
    conn.input.read_exact(&mut version)?;
    // « RFB 003.008\n » → « 3.8 ».
    let announced = String::from_utf8_lossy(&version[4..11])
        .split('.')
        .map(|n| n.trim_start_matches('0').to_owned())
        .map(|n| if n.is_empty() { "0".into() } else { n })
        .collect::<Vec<_>>()
        .join(".");
    let minor = parse_version(&version)
        .ok_or_else(|| protocol_error(tr!("ce n'est pas un serveur VNC", "this is not a VNC server")))?;
    // 3.3, 3.7 ou 3.8 (les serveurs annonçant une version plus récente, 5.0 ou 3.889, acceptent 3.8).
    let minor = match minor {
        ..7 => 3,
        7 => 7,
        _ => 8,
    };
    conn.output.write_all(format!("RFB 003.00{minor}\n").as_bytes())?;
    conn.output.flush()?;

    let kind = if minor == 3 {
        // Le serveur impose son type.
        match conn.input.u32()? {
            0 => return Err(refused(conn)?),
            kind => u8::try_from(kind).map_err(|_| unsupported(encryption))?,
        }
    } else {
        let count = conn.input.u8()?;
        if count == 0 {
            return Err(refused(conn)?);
        }
        let offered = conn.input.read_vec(usize::from(count))?;
        let kind = preference(encryption)
            .iter()
            .copied()
            .find(|k| offered.contains(k))
            .ok_or_else(|| unsupported(encryption))?;
        conn.output.write_all(&[kind])?;
        conn.output.flush()?;
        kind
    };
    if !preference(encryption).contains(&kind) {
        return Err(unsupported(encryption));
    }

    let mut encrypted = false;
    match kind {
        SEC_NONE => {}
        SEC_VNC => {
            let challenge: [u8; 16] = conn.input.read_vec(16)?.try_into().expect("16 octets");
            let (_, password) = auth.credentials(false).ok_or_else(cancelled)?;
            conn.output.write_all(&security::vnc_response(&password, &challenge))?;
            conn.output.flush()?;
        }
        SEC_RA2 | SEC_RA2NE | SEC_RA2_256 | SEC_RA2NE_256 => {
            let all = matches!(kind, SEC_RA2 | SEC_RA2_256);
            security::ra2(conn, matches!(kind, SEC_RA2_256 | SEC_RA2NE_256), all, auth)?;
            encrypted = all;
        }
        SEC_ARD => {
            let (user, password) = auth.credentials(true).ok_or_else(cancelled)?;
            security::ard(conn, &user, &password)?;
        }
        _ => return Err(unsupported(encryption)),
    }

    // Résultat (absent en 3.3 / 3.7 sans authentification).
    if (kind != SEC_NONE || minor == 8) && conn.input.u32()? != 0 {
        let reason = if minor == 8 { read_reason(conn)? } else { String::new() };
        let message = match reason.is_empty() {
            true => tr!("authentification refusée", "authentication failed"),
            false => tr!("authentification refusée : {reason}", "authentication failed: {reason}"),
        };
        return Err(io::Error::new(AUTH_FAILED, message));
    }

    conn.output.write_all(&[u8::from(shared)])?;
    conn.output.flush()?;
    let width = conn.input.u16()?;
    let height = conn.input.u16()?;
    // Format des pixels du serveur : remplacé par le nôtre (SetPixelFormat).
    conn.input.skip(16)?;
    let len = conn.input.u32()?;
    let name = conn.input.read_vec(len.min(1 << 16) as usize)?;
    Ok(ServerInit {
        width,
        height,
        name: String::from_utf8_lossy(&name).into_owned(),
        security: SecurityInfo { kind, encrypted },
        server_version: announced,
        version: format!("3.{minor}"),
    })
}

/// `RFB 003.008\n` → 8 (le numéro mineur, la version majeure étant 3 ou plus).
fn parse_version(version: &[u8; 12]) -> Option<u32> {
    let text = std::str::from_utf8(version).ok()?;
    let rest = text.strip_prefix("RFB ")?.strip_suffix('\n')?;
    let (major, minor) = rest.split_once('.')?;
    let major: u32 = major.parse().ok()?;
    let minor: u32 = minor.parse().ok()?;
    match major {
        3 => Some(minor),
        4.. => Some(8),
        _ => None,
    }
}

fn read_reason(conn: &mut Conn) -> io::Result<String> {
    let len = conn.input.u32()?;
    let reason = conn.input.read_vec(len.min(4096) as usize)?;
    Ok(String::from_utf8_lossy(&reason).into_owned())
}

/// Connexion refusée avant l'authentification, avec la raison donnée par le serveur.
fn refused(conn: &mut Conn) -> io::Result<io::Error> {
    let reason = read_reason(conn)?;
    Ok(protocol_error(tr!(
        "refusé par le serveur : {reason}",
        "refused by the server: {reason}"
    )))
}

fn unsupported(encryption: Encryption) -> io::Error {
    protocol_error(if encryption == Encryption::AlwaysOn {
        tr!(
            "le serveur ne propose pas de chiffrement (réglage « Toujours chiffrer »)",
            "the server does not offer encryption (\"Always on\" setting)"
        )
    } else {
        tr!(
            "aucune méthode de sécurité commune avec le serveur",
            "no security method in common with the server"
        )
    })
}

pub fn cancelled() -> io::Error {
    io::Error::new(CANCELLED, tr!("connexion annulée", "connection cancelled"))
}

// Messages envoyés au serveur.

pub fn set_pixel_format(format: PixelFormat) -> Vec<u8> {
    let mut msg = vec![0, 0, 0, 0];
    msg.extend_from_slice(&format.to_bytes());
    msg
}

pub fn set_encodings(encodings: &[i32]) -> Vec<u8> {
    let mut msg = vec![2, 0];
    msg.extend_from_slice(&(encodings.len() as u16).to_be_bytes());
    for e in encodings {
        msg.extend_from_slice(&e.to_be_bytes());
    }
    msg
}

pub fn update_request(incremental: bool, width: u16, height: u16) -> Vec<u8> {
    let mut msg = vec![3, u8::from(incremental), 0, 0, 0, 0];
    msg.extend_from_slice(&width.to_be_bytes());
    msg.extend_from_slice(&height.to_be_bytes());
    msg
}

pub fn key_event(down: bool, keysym: u32) -> Vec<u8> {
    let mut msg = vec![4, u8::from(down), 0, 0];
    msg.extend_from_slice(&keysym.to_be_bytes());
    msg
}

/// Boutons 1 à 8 dans un octet ; avec les boutons étendus, le bit 7 annonce un second octet qui
/// porte les boutons 8 (« précédent ») et 9 (« suivant »).
pub fn pointer_event(buttons: u16, x: u16, y: u16, extended: bool) -> Vec<u8> {
    let low = if extended {
        (buttons & 0x7f) as u8 | 0x80
    } else {
        buttons as u8
    };
    let mut msg = vec![5, low];
    msg.extend_from_slice(&x.to_be_bytes());
    msg.extend_from_slice(&y.to_be_bytes());
    if extended {
        msg.push((buttons >> 7) as u8);
    }
    msg
}

/// Presse-papiers classique : Latin-1 (les autres caractères deviennent `?`).
pub fn client_cut_text(text: &str) -> Vec<u8> {
    let text: Vec<u8> = text
        .replace("\r\n", "\n")
        .chars()
        .map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?'))
        .collect();
    let mut msg = vec![6, 0, 0, 0];
    msg.extend_from_slice(&(text.len() as u32).to_be_bytes());
    msg.extend_from_slice(&text);
    msg
}

/// Presse-papiers étendu : longueur négative suivie des données.
pub fn client_cut_text_extended(payload: &[u8]) -> Vec<u8> {
    let mut msg = vec![6, 0, 0, 0];
    msg.extend_from_slice(&(-(payload.len() as i32)).to_be_bytes());
    msg.extend_from_slice(payload);
    msg
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(host: &str, port: u16) -> Option<Address> {
        Some(Address {
            host: host.into(),
            port,
        })
    }

    #[test]
    fn addresses() {
        assert_eq!(parse_address("pc"), addr("pc", 5900));
        assert_eq!(parse_address(" pc:1 "), addr("pc", 5901));
        assert_eq!(parse_address("pc:5902"), addr("pc", 5902));
        assert_eq!(parse_address("pc::5"), addr("pc", 5));
        assert_eq!(parse_address("10.0.0.1::443"), addr("10.0.0.1", 443));
        assert_eq!(parse_address("[::1]:2"), addr("::1", 5902));
        assert_eq!(parse_address("[fe80::1]::6000"), addr("fe80::1", 6000));
        assert_eq!(parse_address("fe80::1:2"), addr("fe80::1:2", 5900));
        assert_eq!(parse_address(""), None);
        assert_eq!(parse_address("pc:x"), None);
        assert_eq!(parse_address("p c"), None);
        assert_eq!(addr("pc", 5900).unwrap().to_string(), "pc");
        assert_eq!(addr("::1", 5901).unwrap().to_string(), "[::1]::5901");
        assert_eq!(
            parse_address(&addr("::1", 5901).unwrap().to_string()),
            addr("::1", 5901)
        );
    }

    #[test]
    fn versions() {
        assert_eq!(parse_version(b"RFB 003.008\n"), Some(8));
        assert_eq!(parse_version(b"RFB 003.889\n"), Some(889));
        assert_eq!(parse_version(b"RFB 005.000\n"), Some(8));
        assert_eq!(parse_version(b"RFB 003.003\n"), Some(3));
        assert_eq!(parse_version(b"SSH-2.0-xxxx"), None);
    }

    #[test]
    fn pixel_format_bytes() {
        for f in [PixelFormat::RGB888, PixelFormat::RGB565, PixelFormat::BGR233] {
            assert_eq!(PixelFormat::from_bytes(&f.to_bytes()), f);
        }
        assert_eq!(PixelFormat::RGB565.bytes_per_pixel(), 2);
    }

    #[test]
    fn pointer_buttons() {
        // Sans boutons étendus : le bouton 9 (« suivant ») ne peut pas partir.
        assert_eq!(pointer_event(1 << 7 | 1 << 8, 1, 2, false), [5, 0x80, 0, 1, 0, 2]);
        assert_eq!(pointer_event(1 << 7, 1, 2, true), [5, 0x80, 0, 1, 0, 2, 1]);
    }

    #[test]
    fn latin1_clipboard() {
        let msg = client_cut_text("é€\r\n");
        assert_eq!(&msg[4..8], &3u32.to_be_bytes());
        assert_eq!(&msg[8..], &[0xe9, b'?', b'\n']);
    }
}
