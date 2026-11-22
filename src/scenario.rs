//! Scénarios complets contre un faux serveur VNC : mot de passe VNC, RSA-AES (clé du serveur à
//! accepter, identifiants mémorisés, session chiffrée), Apple, refus du mot de passe, chiffrement
//! exigé mais absent, RFB 3.3 sans sécurité, mises à jour d'écran, curseur, nom, presse-papiers
//! classique et étendu, clavier, souris et boutons étendus.

use std::io::{BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use eframe::egui::Color32;
use flate2::Compression;
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use rand_core::{OsRng, RngCore};
use rsa::{Pkcs1v15Encrypt, RsaPrivateKey};

use crate::config::{Encryption, Settings};
use crate::rfb::{self, Conn, ReadExt};
use crate::security::{self, AesReader, AesWriter, WireKey};
use crate::session::{Answer, Params, Prompt, Session, State, Status, lock};

/// Faux serveur : accepte une connexion et la donne à `serve` dans un thread.
fn server(serve: impl FnOnce(Conn) + Send + 'static) -> (u16, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let thread = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        serve(conn(stream));
    });
    (port, thread)
}

fn conn(stream: TcpStream) -> Conn {
    Conn {
        input: Box::new(BufReader::new(stream.try_clone().unwrap())),
        output: Box::new(stream),
    }
}

fn start(port: u16, settings: Settings, password: Option<&str>, fingerprint: &str) -> Session {
    let params = Params {
        address: rfb::Address {
            host: "127.0.0.1".into(),
            port,
        },
        settings,
        username: String::new(),
        password: password.map(Into::into),
        fingerprint: fingerprint.into(),
    };
    Session::start(params, || {})
}

/// Attend que l'état de la session vérifie `test`.
fn wait<T>(session: &Session, what: &str, mut test: impl FnMut(&mut State) -> Option<T>) -> T {
    let start = Instant::now();
    loop {
        if let Some(value) = test(&mut lock(&session.state)) {
            return value;
        }
        let status = lock(&session.state).status.clone();
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "attendu : {what} ({status:?})"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn write(conn: &mut Conn, data: &[u8]) {
    conn.output.write_all(data).unwrap();
    conn.output.flush().unwrap();
}

/// Version 3.8 et choix du type de sécurité parmi `offered`.
fn negotiate(conn: &mut Conn, offered: &[u8]) -> u8 {
    write(conn, b"RFB 003.008\n");
    let mut version = [0; 12];
    conn.input.read_exact(&mut version).unwrap();
    assert_eq!(&version, b"RFB 003.008\n");
    let mut msg = vec![offered.len() as u8];
    msg.extend_from_slice(offered);
    write(conn, &msg);
    conn.input.u8().unwrap()
}

fn server_init(conn: &mut Conn, width: u16, height: u16, name: &str) {
    let shared = conn.input.u8().unwrap();
    assert_eq!(shared, 1);
    let mut msg = width.to_be_bytes().to_vec();
    msg.extend_from_slice(&height.to_be_bytes());
    msg.extend_from_slice(&rfb::PixelFormat::RGB565.to_bytes());
    msg.extend_from_slice(&(name.len() as u32).to_be_bytes());
    msg.extend_from_slice(name.as_bytes());
    write(conn, &msg);
}

/// Lit SetPixelFormat, SetEncodings et la première demande de mise à jour.
fn client_setup(conn: &mut Conn) -> Vec<i32> {
    assert_eq!(conn.input.u8().unwrap(), 0);
    conn.input.skip(3).unwrap();
    let format: [u8; 16] = conn.input.read_vec(16).unwrap().try_into().unwrap();
    assert_eq!(rfb::PixelFormat::from_bytes(&format), rfb::PixelFormat::RGB888);
    assert_eq!(conn.input.u8().unwrap(), 2);
    conn.input.skip(1).unwrap();
    let count = conn.input.u16().unwrap();
    let encodings = (0..count).map(|_| conn.input.i32().unwrap()).collect();
    expect_update_request(conn, false);
    encodings
}

fn expect_update_request(conn: &mut Conn, incremental: bool) {
    assert_eq!(conn.input.u8().unwrap(), 3);
    assert_eq!(conn.input.u8().unwrap(), u8::from(incremental));
    conn.input.skip(8).unwrap();
}

/// Rectangle d'une mise à jour : x, y, largeur, hauteur, encodage, données.
type Rect = (u16, u16, u16, u16, i32, Vec<u8>);

fn update(rects: &[Rect]) -> Vec<u8> {
    let mut msg = vec![0, 0];
    msg.extend_from_slice(&(rects.len() as u16).to_be_bytes());
    for (x, y, w, h, encoding, data) in rects {
        for v in [x, y, w, h] {
            msg.extend_from_slice(&v.to_be_bytes());
        }
        msg.extend_from_slice(&encoding.to_be_bytes());
        msg.extend_from_slice(data);
    }
    msg
}

/// Pixel RGB888 petit-boutiste (format demandé par le client).
fn px(r: u8, g: u8, b: u8) -> [u8; 4] {
    [b, g, r, 0]
}

#[test]
fn vnc_password_session() {
    let (port, server) = server(|mut conn| {
        assert_eq!(negotiate(&mut conn, &[rfb::SEC_VNC, 99]), rfb::SEC_VNC);
        let challenge = [7u8; 16];
        write(&mut conn, &challenge);
        let response: [u8; 16] = conn.input.read_vec(16).unwrap().try_into().unwrap();
        assert_eq!(response, security::vnc_response("secret", &challenge));
        write(&mut conn, &0u32.to_be_bytes());
        server_init(&mut conn, 4, 2, "Bureau test");
        let encodings = client_setup(&mut conn);
        assert!(encodings.contains(&crate::decode::ZRLE) && encodings.contains(&crate::decode::CURSOR));

        let mut raw = Vec::new();
        for i in 0..8u8 {
            raw.extend_from_slice(&px(i * 30, 0, 255));
        }
        let cursor = [px(255, 0, 0), px(255, 0, 0)].concat();
        let mut cursor_data = cursor.clone();
        cursor_data.extend_from_slice(&[0x80]);
        let name = "Nouveau nom";
        let mut name_data = (name.len() as u32).to_be_bytes().to_vec();
        name_data.extend_from_slice(name.as_bytes());
        let msg = update(&[
            (0, 0, 4, 2, crate::decode::RAW, raw),
            (0, 0, 2, 1, crate::decode::CURSOR, cursor_data),
            (0, 0, 0, 0, crate::decode::DESKTOP_NAME, name_data),
            (0, 0, 0, 0, crate::decode::EXTENDED_MOUSE_BUTTONS, vec![]),
        ]);
        write(&mut conn, &msg);
        expect_update_request(&mut conn, true);
        // Presse-papiers classique (Latin-1).
        let mut cut = vec![3, 0, 0, 0];
        cut.extend_from_slice(&5u32.to_be_bytes());
        cut.extend_from_slice(&[b'c', 0xe9, b'l', b'a', b'n']);
        write(&mut conn, &cut);
        write(&mut conn, &[2]); // sonnerie

        // Clavier et souris envoyés par l'interface.
        assert_eq!(conn.input.read_vec(8).unwrap(), rfb::key_event(true, 0x61));
        assert_eq!(
            conn.input.read_vec(7).unwrap(),
            rfb::pointer_event(1 << 8 | 1, 3, 1, true)
        );
        assert_eq!(rfb::pointer_event(1 << 8 | 1, 3, 1, true), [5, 0x81, 0, 3, 0, 1, 2]);
        assert_eq!(conn.input.read_vec(10).unwrap(), rfb::client_cut_text("é\r\n"));
        // Agrandissement de l'écran : demande complète.
        write(&mut conn, &update(&[(0, 0, 8, 8, crate::decode::DESKTOP_SIZE, vec![])]));
        expect_update_request(&mut conn, false);
    });
    let settings = Settings::default();
    let session = start(port, settings, Some("secret"), "");
    wait(&session, "première image", |st| {
        (st.status == Status::Connected && st.fb.pixels.get(7) == Some(&Color32::from_rgb(210, 0, 255))).then_some(())
    });
    {
        let st = lock(&session.state);
        assert_eq!(st.name, "Nouveau nom");
        assert_eq!(
            (st.info.protocol.as_str(), st.info.server_protocol.as_str()),
            ("3.8", "3.8")
        );
        let cursor = st.cursor.as_ref().expect("curseur");
        assert_eq!((cursor.width, cursor.height), (2, 1));
        assert_eq!(&cursor.rgba[..8], &[255, 0, 0, 255, 255, 0, 0, 0]);
        assert_eq!(st.security.unwrap().kind, rfb::SEC_VNC);
        assert!(!st.security.unwrap().encrypted);
    }
    let text = wait(&session, "presse-papiers", |st| st.clipboard_in.take());
    assert_eq!(text, "célan");
    wait(&session, "sonnerie", |st| st.bell.then_some(()));
    session.key(true, 0x61);
    session.pointer(1 << 8 | 1, 3, 1);
    session.clipboard("é\n");
    wait(&session, "nouvelle taille", |st| (st.fb.width == 8).then_some(()));
    server.join().unwrap();
}

#[test]
fn rsa_aes_session() {
    let (port, server) = server(|mut conn| {
        assert_eq!(
            negotiate(&mut conn, &[rfb::SEC_VNC, rfb::SEC_RA2_256]),
            rfb::SEC_RA2_256
        );
        let key = RsaPrivateKey::new(&mut OsRng, 1024).unwrap();
        let server = WireKey::of(&key.to_public_key());
        server.write(&mut conn.output).unwrap();
        conn.output.flush().unwrap();
        let client = WireKey::read(&mut conn.input).unwrap();
        assert_eq!(client.bits, 1024);
        let len = usize::from(conn.input.u16().unwrap());
        let client_random = key
            .decrypt(Pkcs1v15Encrypt, &conn.input.read_vec(len).unwrap())
            .unwrap();
        let mut server_random = [0u8; 32];
        OsRng.fill_bytes(&mut server_random);
        let encrypted = client
            .public()
            .unwrap()
            .encrypt(&mut OsRng, Pkcs1v15Encrypt, &server_random)
            .unwrap();
        write(&mut conn, &(encrypted.len() as u16).to_be_bytes());
        write(&mut conn, &encrypted);
        let (read_key, write_key) = security::session_keys(true, &server_random, &client_random);
        let raw_input = std::mem::replace(&mut conn.input, Box::new(std::io::empty()));
        let raw_output = std::mem::replace(&mut conn.output, Box::new(std::io::sink()));
        let mut conn = Conn {
            input: Box::new(AesReader::new(raw_input, &read_key)),
            output: Box::new(AesWriter::new(raw_output, &write_key)),
        };
        write(&mut conn, &security::keys_hash(true, &server, &client));
        assert_eq!(
            conn.input.read_vec(32).unwrap(),
            security::keys_hash(true, &client, &server)
        );
        write(&mut conn, &[1]); // nom d'utilisateur et mot de passe
        let user = conn.input.u8().unwrap();
        assert_eq!(conn.input.read_vec(user.into()).unwrap(), b"moi");
        let password = conn.input.u8().unwrap();
        assert_eq!(conn.input.read_vec(password.into()).unwrap(), "mot de passé".as_bytes());
        write(&mut conn, &0u32.to_be_bytes());
        server_init(&mut conn, 16, 16, "Chiffré");
        client_setup(&mut conn);
        // Tight : remplissage rouge, chiffré comme tout le reste.
        write(
            &mut conn,
            &update(&[(0, 0, 16, 16, crate::decode::TIGHT, vec![0x80, 255, 0, 0])]),
        );
        expect_update_request(&mut conn, true);

        // Presse-papiers étendu : capacités, puis texte annoncé, demandé et fourni.
        let mut caps = (1u32 << 24 | 1 << 25 | 1 << 27 | 1 << 28 | 1).to_be_bytes().to_vec();
        caps.extend_from_slice(&(1u32 << 20).to_be_bytes());
        write(&mut conn, &extended(&caps));
        let reply = read_extended(&mut conn);
        assert_eq!(
            u32::from_be_bytes(reply[..4].try_into().unwrap()) & (1 << 24 | 1),
            1 << 24 | 1
        );
        write(&mut conn, &extended(&(1u32 << 27 | 1).to_be_bytes()));
        let request = read_extended(&mut conn);
        assert_eq!(request, (1u32 << 25 | 1).to_be_bytes());
        let text = "héllo €\r\n";
        let mut plain = (text.len() as u32 + 1).to_be_bytes().to_vec();
        plain.extend_from_slice(text.as_bytes());
        plain.push(0);
        let mut z = ZlibEncoder::new(Vec::new(), Compression::default());
        z.write_all(&plain).unwrap();
        let mut provide = (1u32 << 28 | 1).to_be_bytes().to_vec();
        provide.extend_from_slice(&z.finish().unwrap());
        write(&mut conn, &extended(&provide));
        // Texte de ce PC : annoncé, demandé par le serveur, fourni.
        assert_eq!(read_extended(&mut conn), (1u32 << 27 | 1).to_be_bytes());
        write(&mut conn, &extended(&(1u32 << 25 | 1).to_be_bytes()));
        let provided = read_extended(&mut conn);
        assert_eq!(&provided[..4], &(1u32 << 28 | 1).to_be_bytes());
        let mut plain = Vec::new();
        ZlibDecoder::new(&provided[4..]).read_to_end(&mut plain).unwrap();
        assert_eq!(&plain[4..], "ça\r\nva\0".as_bytes());
    });
    let session = start(port, Settings::default(), None, "");
    let fingerprint = wait(&session, "empreinte", |st| match &st.prompt {
        Some(Prompt::Fingerprint { fingerprint, known }) => {
            assert!(known.is_empty());
            Some(fingerprint.clone())
        }
        _ => None,
    });
    session.answer(Answer::Accept);
    wait(&session, "identifiants", |st| {
        (st.prompt == Some(Prompt::Credentials { username: true })).then_some(())
    });
    session.answer(Answer::Credentials {
        user: "moi".into(),
        password: "mot de passé".into(),
        remember: true,
    });
    wait(&session, "image chiffrée", |st| {
        (st.fb.pixels.first() == Some(&Color32::from_rgb(255, 0, 0))).then_some(())
    });
    {
        let mut st = lock(&session.state);
        assert!(st.security.unwrap().encrypted);
        assert_eq!(st.save_fingerprint.take(), Some(fingerprint));
        assert_eq!(st.save_credentials.take(), Some(("moi".into(), "mot de passé".into())));
    }
    let text = wait(&session, "presse-papiers étendu", |st| st.clipboard_in.take());
    assert_eq!(text, "héllo €\n");
    session.clipboard("ça\nva");
    server.join().unwrap();
}

fn extended(payload: &[u8]) -> Vec<u8> {
    let mut msg = vec![3, 0, 0, 0];
    msg.extend_from_slice(&(-(payload.len() as i32)).to_be_bytes());
    msg.extend_from_slice(payload);
    msg
}

fn read_extended(conn: &mut Conn) -> Vec<u8> {
    assert_eq!(conn.input.u8().unwrap(), 6);
    conn.input.skip(3).unwrap();
    let len = conn.input.i32().unwrap();
    assert!(len < 0);
    conn.input.read_vec(len.unsigned_abs() as usize).unwrap()
}

#[test]
fn refused_password() {
    let (port, server) = server(|mut conn| {
        negotiate(&mut conn, &[rfb::SEC_VNC]);
        write(&mut conn, &[0; 16]);
        conn.input.read_vec(16).unwrap();
        let reason = b"Mauvais mot de passe";
        let mut msg = 1u32.to_be_bytes().to_vec();
        msg.extend_from_slice(&(reason.len() as u32).to_be_bytes());
        msg.extend_from_slice(reason);
        write(&mut conn, &msg);
    });
    let session = start(port, Settings::default(), Some("faux"), "");
    let closed = wait(&session, "refus", |st| match &st.status {
        Status::Closed(c) => Some(c.clone()),
        _ => None,
    });
    assert!(closed.auth_failed);
    assert!(closed.message.contains("Mauvais mot de passe"), "{}", closed.message);
    server.join().unwrap();
}

#[test]
fn old_server_without_security() {
    let (port, server) = server(|mut conn| {
        write(&mut conn, b"RFB 003.003\n");
        assert_eq!(conn.input.read_vec(12).unwrap(), b"RFB 003.003\n");
        write(&mut conn, &1u32.to_be_bytes());
        server_init(&mut conn, 2, 2, "Ancien");
        client_setup(&mut conn);
        // Hextile puis ZRLE dans la même mise à jour.
        let mut z = ZlibEncoder::new(Vec::new(), Compression::default());
        z.write_all(&[1, 0, 255, 0]).unwrap();
        let block = z.finish().unwrap();
        let mut zrle = (block.len() as u32).to_be_bytes().to_vec();
        zrle.extend_from_slice(&block);
        let mut hextile = vec![2];
        hextile.extend_from_slice(&px(0, 0, 255));
        write(
            &mut conn,
            &update(&[
                (0, 0, 2, 2, crate::decode::HEXTILE, hextile),
                (1, 1, 1, 1, crate::decode::ZRLE, zrle),
            ]),
        );
        expect_update_request(&mut conn, true);
    });
    let session = start(port, Settings::default(), None, "");
    wait(&session, "image", |st| {
        (st.fb.pixels == [Color32::BLUE, Color32::BLUE, Color32::BLUE, Color32::GREEN]).then_some(())
    });
    assert_eq!(lock(&session.state).security.unwrap().kind, rfb::SEC_NONE);
    assert_eq!(lock(&session.state).info.protocol, "3.3");
    server.join().unwrap();
}

#[test]
fn encryption_required() {
    let (port, server) = server(|mut conn| {
        write(&mut conn, b"RFB 003.008\n");
        conn.input.read_vec(12).unwrap();
        write(&mut conn, &[1, rfb::SEC_VNC]);
        // Le client raccroche sans choisir.
        assert!(conn.input.u8().is_err());
    });
    let settings = Settings {
        encryption: Encryption::AlwaysOn,
        ..Settings::default()
    };
    let session = start(port, settings, None, "");
    let closed = wait(&session, "refus", |st| match &st.status {
        Status::Closed(c) => Some(c.clone()),
        _ => None,
    });
    assert!(closed.message.contains("chiffrement"), "{}", closed.message);
    server.join().unwrap();
}

#[test]
fn apple_session() {
    use aes::Aes128;
    use aes::cipher::generic_array::GenericArray;
    use aes::cipher::{BlockDecrypt, KeyInit};
    use md5::{Digest, Md5};
    use rsa::BigUint;

    let (port, server) = server(|mut conn| {
        assert_eq!(negotiate(&mut conn, &[rfb::SEC_ARD]), rfb::SEC_ARD);
        // Le calcul est le même pour tout module : un nombre impair quelconque suffit au test.
        let mut prime = [0xc5u8; 128];
        prime[127] |= 1;
        let prime = BigUint::from_bytes_be(&prime);
        let private = BigUint::from(123_456_789u64);
        let public = BigUint::from(2u8).modpow(&private, &prime);
        let mut msg = vec![0, 2, 0, 128];
        msg.extend_from_slice(&prime.to_bytes_be());
        let mut public_bytes = vec![0; 128 - public.to_bytes_be().len()];
        public_bytes.extend_from_slice(&public.to_bytes_be());
        msg.extend_from_slice(&public_bytes);
        write(&mut conn, &msg);
        let mut credentials = conn.input.read_vec(128).unwrap();
        let client = BigUint::from_bytes_be(&conn.input.read_vec(128).unwrap());
        let secret = client.modpow(&private, &prime).to_bytes_be();
        let mut padded = vec![0; 128 - secret.len()];
        padded.extend_from_slice(&secret);
        let cipher = Aes128::new(&Md5::digest(&padded));
        for block in credentials.as_chunks_mut::<16>().0 {
            cipher.decrypt_block(GenericArray::from_mut_slice(block));
        }
        assert_eq!(&credentials[..4], b"moi\0");
        assert_eq!(&credentials[64..71], b"secret\0");
        write(&mut conn, &0u32.to_be_bytes());
        server_init(&mut conn, 1, 1, "Mac");
        client_setup(&mut conn);
    });
    let session = start(port, Settings::default(), None, "");
    wait(&session, "identifiants", |st| {
        (st.prompt == Some(Prompt::Credentials { username: true })).then_some(())
    });
    session.answer(Answer::Credentials {
        user: "moi".into(),
        password: "secret".into(),
        remember: false,
    });
    server.join().unwrap();
    let st = lock(&session.state);
    assert_eq!(st.security.unwrap().kind, rfb::SEC_ARD);
    assert_eq!(st.save_credentials, None);
}
