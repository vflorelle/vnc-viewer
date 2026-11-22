//! Authentifications : mot de passe VNC (DES), RSA-AES (« RA2 » et ses variantes)
//! et Apple (Diffie-Hellman). Flux chiffrés AES-EAX de RA2.

use std::io::{self, Read, Write};

use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockEncrypt, KeyInit};
use aes::{Aes128, Aes256};
use eax::Eax;
use eax::aead::AeadInPlace;
use md5::Md5;
use rand_core::{OsRng, RngCore};
use rsa::traits::PublicKeyParts;
use rsa::{BigUint, Pkcs1v15Encrypt, RsaPrivateKey, RsaPublicKey};
use sha1::{Digest, Sha1};
use sha2::Sha256;

use crate::rfb::{AUTH_FAILED, Auth, Conn, ReadExt, cancelled, protocol_error};

/// Réponse au défi du mot de passe VNC : le défi chiffré en DES, la clé étant les 8 premiers
/// octets du mot de passe, chacun aux bits inversés.
pub fn vnc_response(password: &str, challenge: &[u8; 16]) -> [u8; 16] {
    let mut key = [0u8; 8];
    for (k, b) in key.iter_mut().zip(password.bytes()) {
        *k = b.reverse_bits();
    }
    let cipher = des::Des::new(&key.into());
    let mut response = *challenge;
    for block in response.as_chunks_mut::<8>().0 {
        cipher.encrypt_block(GenericArray::from_mut_slice(block));
    }
    response
}

/// Empreinte d'une clé RSA du serveur, au format affiché par les serveurs (`xx-xx-…`, 8 octets
/// du SHA-1 de la clé).
pub fn fingerprint(bits: u32, n: &[u8], e: &[u8]) -> String {
    let hash = Sha1::new()
        .chain_update(bits.to_be_bytes())
        .chain_update(n)
        .chain_update(e)
        .finalize();
    hash[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join("-")
}

const MIN_KEY_BITS: u32 = 1024;
const MAX_KEY_BITS: u32 = 8192;

/// Clé RSA publique telle qu'échangée : taille en bits, puis module et exposant sur autant
/// d'octets.
pub struct WireKey {
    pub bits: u32,
    pub n: Vec<u8>,
    pub e: Vec<u8>,
}

impl WireKey {
    pub fn of(key: &RsaPublicKey) -> Self {
        let bits = key.n().bits() as u32;
        let size = bits.div_ceil(8) as usize;
        WireKey {
            bits,
            n: padded(key.n(), size),
            e: padded(key.e(), size),
        }
    }

    pub fn read(input: &mut dyn Read) -> io::Result<Self> {
        let bits = input.u32()?;
        if !(MIN_KEY_BITS..=MAX_KEY_BITS).contains(&bits) {
            return Err(protocol_error(tr!(
                "clé du serveur de taille invalide ({bits} bits)",
                "invalid server key size ({bits} bits)"
            )));
        }
        let size = bits.div_ceil(8) as usize;
        let n = input.read_vec(size)?;
        let e = input.read_vec(size)?;
        Ok(WireKey { bits, n, e })
    }

    pub fn write(&self, output: &mut dyn Write) -> io::Result<()> {
        output.write_all(&self.bits.to_be_bytes())?;
        output.write_all(&self.n)?;
        output.write_all(&self.e)
    }

    pub fn public(&self) -> io::Result<RsaPublicKey> {
        RsaPublicKey::new_with_max_size(
            BigUint::from_bytes_be(&self.n),
            BigUint::from_bytes_be(&self.e),
            MAX_KEY_BITS as usize,
        )
        .map_err(|e| protocol_error(tr!("clé du serveur invalide ({e})", "invalid server key ({e})")))
    }

    fn hash(&self, hasher: &mut impl Digest) {
        hasher.update(self.bits.to_be_bytes());
        hasher.update(&self.n);
        hasher.update(&self.e);
    }
}

fn padded(value: &BigUint, size: usize) -> Vec<u8> {
    let bytes = value.to_bytes_be();
    let mut out = vec![0; size.saturating_sub(bytes.len())];
    out.extend_from_slice(&bytes);
    out
}

/// Clés AES des deux sens, dérivées des deux nombres aléatoires échangés :
/// (lecture, écriture) vues de celui qui a tiré `own`.
pub fn session_keys(key256: bool, own: &[u8], other: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let derive = |a: &[u8], b: &[u8]| -> Vec<u8> {
        if key256 {
            Sha256::new().chain_update(a).chain_update(b).finalize().to_vec()
        } else {
            Sha1::new().chain_update(a).chain_update(b).finalize()[..16].to_vec()
        }
    };
    // Le client lit avec H(aléa client, aléa serveur) et écrit avec H(aléa serveur, aléa client).
    (derive(own, other), derive(other, own))
}

/// Preuve que les deux côtés ont vu les mêmes clés publiques : `first` puis `second`.
pub fn keys_hash(key256: bool, first: &WireKey, second: &WireKey) -> Vec<u8> {
    if key256 {
        let mut h = Sha256::new();
        first.hash(&mut h);
        second.hash(&mut h);
        h.finalize().to_vec()
    } else {
        let mut h = Sha1::new();
        first.hash(&mut h);
        second.hash(&mut h);
        h.finalize().to_vec()
    }
}

/// RSA-AES : échange de clés RSA, flux AES-EAX, puis identifiants. Si `all`, toute la suite de
/// la session reste chiffrée ; sinon (variantes « ne ») seule l'authentification l'est.
pub fn ra2(conn: &mut Conn, key256: bool, all: bool, auth: &mut dyn Auth) -> io::Result<()> {
    let server = WireKey::read(&mut conn.input)?;
    let server_key = server.public()?;
    if !auth.verify(&fingerprint(server.bits, &server.n, &server.e)) {
        return Err(cancelled());
    }
    // Clé du client de la même taille que celle du serveur, à usage unique.
    let client_key =
        RsaPrivateKey::new(&mut OsRng, server.bits as usize).map_err(|e| io::Error::other(format!("RSA : {e}")))?;
    let client = WireKey::of(&client_key.to_public_key());
    client.write(&mut conn.output)?;

    let random_len = if key256 { 32 } else { 16 };
    let mut client_random = vec![0; random_len];
    OsRng.fill_bytes(&mut client_random);
    let encrypted = server_key
        .encrypt(&mut OsRng, Pkcs1v15Encrypt, &client_random)
        .map_err(|e| io::Error::other(format!("RSA : {e}")))?;
    conn.output.write_all(&(encrypted.len() as u16).to_be_bytes())?;
    conn.output.write_all(&encrypted)?;
    conn.output.flush()?;

    let len = usize::from(conn.input.u16()?);
    if len != client.n.len() {
        return Err(protocol_error(tr!(
            "aléa du serveur de taille invalide",
            "invalid server random size"
        )));
    }
    let server_random = client_key
        .decrypt(Pkcs1v15Encrypt, &conn.input.read_vec(len)?)
        .ok()
        .filter(|r| r.len() == random_len)
        .ok_or_else(|| protocol_error(tr!("aléa du serveur illisible", "unreadable server random")))?;

    let (read_key, write_key) = session_keys(key256, &client_random, &server_random);
    let raw_input = std::mem::replace(&mut conn.input, Box::new(io::empty()));
    let raw_output = std::mem::replace(&mut conn.output, Box::new(io::sink()));
    let mut input = AesReader::new(raw_input, &read_key);
    let mut output = AesWriter::new(raw_output, &write_key);

    output.write_all(&keys_hash(key256, &client, &server))?;
    output.flush()?;
    let expected = keys_hash(key256, &server, &client);
    if input.read_vec(expected.len())? != expected {
        return Err(protocol_error(tr!(
            "échange de clés falsifié (empreintes différentes)",
            "tampered key exchange (hash mismatch)"
        )));
    }

    // 1 : nom d'utilisateur et mot de passe ; 2 : mot de passe seul.
    let username = match input.u8()? {
        1 => true,
        2 => false,
        other => {
            return Err(protocol_error(tr!(
                "variante RSA-AES inconnue ({other})",
                "unknown RSA-AES subtype ({other})"
            )));
        }
    };
    let (user, password) = auth.credentials(username).ok_or_else(cancelled)?;
    if user.len() > 255 || password.len() > 255 {
        return Err(io::Error::new(
            AUTH_FAILED,
            tr!("identifiants trop longs", "credentials too long"),
        ));
    }
    let user = if username { user.as_bytes() } else { &[] };
    output.write_all(&[user.len() as u8])?;
    output.write_all(user)?;
    output.write_all(&[password.len() as u8])?;
    output.write_all(password.as_bytes())?;
    output.flush()?;

    if all {
        conn.input = Box::new(input);
        conn.output = Box::new(output);
    } else {
        conn.input = input.into_inner()?;
        conn.output = output.inner;
    }
    Ok(())
}

/// Authentification Apple (partage d'écran de macOS) : Diffie-Hellman, puis identifiants du
/// compte macOS chiffrés en AES-128 avec le MD5 du secret commun.
pub fn ard(conn: &mut Conn, user: &str, password: &str) -> io::Result<()> {
    let generator = BigUint::from(conn.input.u16()?);
    let len = usize::from(conn.input.u16()?);
    if len == 0 || len > 1024 {
        return Err(protocol_error(tr!(
            "clé Diffie-Hellman invalide",
            "invalid Diffie-Hellman key"
        )));
    }
    let prime = BigUint::from_bytes_be(&conn.input.read_vec(len)?);
    let server_public = BigUint::from_bytes_be(&conn.input.read_vec(len)?);
    let mut private = vec![0; len];
    OsRng.fill_bytes(&mut private);
    let private = BigUint::from_bytes_be(&private);
    let public = generator.modpow(&private, &prime);
    let secret = server_public.modpow(&private, &prime);
    let key = Md5::digest(padded(&secret, len));

    // Nom puis mot de passe, chacun dans 64 octets terminés par un zéro, le reste aléatoire.
    let mut credentials = [0u8; 128];
    OsRng.fill_bytes(&mut credentials);
    for (field, text) in credentials.as_chunks_mut::<64>().0.iter_mut().zip([user, password]) {
        let text = &text.as_bytes()[..text.len().min(63)];
        field[..text.len()].copy_from_slice(text);
        field[text.len()] = 0;
    }
    let cipher = Aes128::new(&key);
    for block in credentials.as_chunks_mut::<16>().0 {
        cipher.encrypt_block(GenericArray::from_mut_slice(block));
    }
    conn.output.write_all(&credentials)?;
    conn.output.write_all(&padded(&public, len))?;
    conn.output.flush()
}

/// AES-EAX à 128 ou 256 bits.
enum Cipher {
    Aes128(Box<Eax<Aes128>>),
    Aes256(Box<Eax<Aes256>>),
}

impl Cipher {
    fn new(key: &[u8]) -> Self {
        match key.len() {
            32 => Cipher::Aes256(Box::new(Eax::new(GenericArray::from_slice(key)))),
            _ => Cipher::Aes128(Box::new(Eax::new(GenericArray::from_slice(key)))),
        }
    }
}

/// Numéro de message : entier de 128 bits petit-boutiste, nonce de chaque message.
fn increment(counter: &mut [u8; 16]) {
    for b in counter {
        *b = b.wrapping_add(1);
        if *b != 0 {
            break;
        }
    }
}

const TAG_LEN: usize = 16;

/// Flux entrant chiffré : messages `longueur (2 octets, données associées) | données | tag`.
pub struct AesReader {
    inner: Box<dyn Read + Send>,
    cipher: Cipher,
    counter: [u8; 16],
    plain: Vec<u8>,
    pos: usize,
}

impl AesReader {
    pub fn new(inner: Box<dyn Read + Send>, key: &[u8]) -> Self {
        AesReader {
            inner,
            cipher: Cipher::new(key),
            counter: [0; 16],
            plain: Vec::new(),
            pos: 0,
        }
    }

    /// Le flux en clair, une fois tout le déchiffré consommé.
    fn into_inner(self) -> io::Result<Box<dyn Read + Send>> {
        if self.pos < self.plain.len() {
            return Err(protocol_error(tr!(
                "données chiffrées inattendues",
                "unexpected encrypted data"
            )));
        }
        Ok(self.inner)
    }

    fn fill(&mut self) -> io::Result<()> {
        let mut len = [0; 2];
        self.inner.read_exact(&mut len)?;
        let size = usize::from(u16::from_be_bytes(len));
        let mut data = vec![0; size + TAG_LEN];
        self.inner.read_exact(&mut data)?;
        let tag = data.split_off(size);
        let nonce = GenericArray::from(self.counter);
        let tag = GenericArray::from_slice(&tag);
        let ok = match &self.cipher {
            Cipher::Aes128(c) => c.decrypt_in_place_detached(&nonce, &len, &mut data, tag),
            Cipher::Aes256(c) => c.decrypt_in_place_detached(&nonce, &len, &mut data, tag),
        };
        ok.map_err(|_| protocol_error(tr!("message chiffré altéré", "corrupted encrypted message")))?;
        increment(&mut self.counter);
        self.plain = data;
        self.pos = 0;
        Ok(())
    }
}

impl Read for AesReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while self.pos == self.plain.len() {
            if buf.is_empty() {
                return Ok(0);
            }
            self.fill()?;
        }
        let n = buf.len().min(self.plain.len() - self.pos);
        buf[..n].copy_from_slice(&self.plain[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/// Flux sortant chiffré : les données écrites partent en messages à chaque `flush`.
pub struct AesWriter {
    inner: Box<dyn Write + Send>,
    cipher: Cipher,
    counter: [u8; 16],
    pending: Vec<u8>,
}

/// Taille maximale des données d'un message.
const MAX_MESSAGE: usize = 8192;

impl AesWriter {
    pub fn new(inner: Box<dyn Write + Send>, key: &[u8]) -> Self {
        AesWriter {
            inner,
            cipher: Cipher::new(key),
            counter: [0; 16],
            pending: Vec::new(),
        }
    }
}

impl Write for AesWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.pending.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        let pending = std::mem::take(&mut self.pending);
        for chunk in pending.chunks(MAX_MESSAGE) {
            let len = (chunk.len() as u16).to_be_bytes();
            let mut data = chunk.to_vec();
            let nonce = GenericArray::from(self.counter);
            let tag = match &self.cipher {
                Cipher::Aes128(c) => c.encrypt_in_place_detached(&nonce, &len, &mut data),
                Cipher::Aes256(c) => c.encrypt_in_place_detached(&nonce, &len, &mut data),
            }
            .map_err(|_| io::Error::other("AES"))?;
            increment(&mut self.counter);
            let mut message = Vec::with_capacity(2 + data.len() + TAG_LEN);
            message.extend_from_slice(&len);
            message.extend_from_slice(&data);
            message.extend_from_slice(&tag);
            self.inner.write_all(&message)?;
        }
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vnc_password() {
        // Valeur de référence : défi de zéros, mot de passe « password ».
        let response = vnc_response("password", &[0; 16]);
        let expected = vnc_response("password\u{0}ignoré", &[0; 16]);
        assert_eq!(response, expected);
        assert_ne!(response, vnc_response("passwore", &[0; 16]));
        assert_eq!(response[..8], response[8..]);
    }

    #[test]
    fn counter_carry() {
        let mut c = [0xff; 16];
        c[2] = 7;
        increment(&mut c);
        assert_eq!(&c[..3], &[0, 0, 8]);
        assert_eq!(c[3], 0xff);
    }

    #[test]
    fn aes_stream() {
        let wire = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        struct Shared(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
        impl Write for Shared {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        for key in [vec![7u8; 16], vec![9u8; 32]] {
            wire.lock().unwrap().clear();
            let mut writer = AesWriter::new(Box::new(Shared(wire.clone())), &key);
            let big: Vec<u8> = (0..20000u32).map(|i| i as u8).collect();
            writer.write_all(b"bonjour").unwrap();
            writer.flush().unwrap();
            writer.write_all(&big).unwrap();
            writer.flush().unwrap();
            let bytes = wire.lock().unwrap().clone();
            // 1 message, puis 3 (8192 + 8192 + 3616).
            assert_eq!(bytes.len(), 7 + 20000 + 4 * (2 + TAG_LEN));
            let mut reader = AesReader::new(Box::new(io::Cursor::new(bytes.clone())), &key);
            assert_eq!(reader.read_vec(7).unwrap(), b"bonjour");
            assert_eq!(reader.read_vec(20000).unwrap(), big);
            // Un octet modifié est détecté.
            let mut bad = bytes;
            bad[3] ^= 1;
            let mut reader = AesReader::new(Box::new(io::Cursor::new(bad)), &key);
            assert!(reader.u8().is_err());
        }
    }

    #[test]
    fn fingerprint_format() {
        let fp = fingerprint(1024, &[1; 128], &[2; 128]);
        assert_eq!(fp.len(), 8 * 3 - 1);
        assert_eq!(fp.matches('-').count(), 7);
    }
}
