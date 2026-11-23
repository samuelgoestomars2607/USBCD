//! Gegenseitige Anmeldung zwischen PC und Tablet-App.
//!
//! Beim Start der App über adb (`am start … --es usbcd_token <hex>`) bekommt sie ein
//! zufälliges Geheimnis, das nur USBCD auf dem PC kennt (es liegt nirgends auf der
//! Festplatte und gilt, solange USBCD läuft). Andere Apps auf dem Tablet können den
//! Anschluss zwar belegen, aber ohne das Geheimnis weder das Bild empfangen noch eins
//! einspielen:
//!
//!   App → PC  CHALLENGE  nonce_a (32 Byte)
//!   PC → App  AUTH       nonce_p (32) ‖ HMAC(token, "USBCD1 pc"  ‖ nonce_a ‖ nonce_p)
//!   App → PC  AUTH_OK    HMAC(token, "USBCD1 app" ‖ nonce_a ‖ nonce_p)

use crate::proto::{read_frame_max, write_frame, T_AUTH, T_AUTH_OK, T_CHALLENGE, T_HELLO};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::io::{Read, Write};
use std::sync::OnceLock;

type HmacSha256 = Hmac<Sha256>;
pub const NONCE: usize = 32;
pub const TAG: usize = 32;

pub fn random<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom::getrandom(&mut b).expect("Zufallsquelle nicht verfügbar");
    b
}

static TOKEN: OnceLock<[u8; 32]> = OnceLock::new();

/// Geheimnis dieser USBCD-Sitzung.
pub fn token() -> &'static [u8; 32] {
    TOKEN.get_or_init(random::<32>)
}

pub fn token_hex() -> String {
    hex(token())
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn mac(key: &[u8], label: &[u8], na: &[u8], np: &[u8]) -> HmacSha256 {
    let mut m = HmacSha256::new_from_slice(key).expect("HMAC akzeptiert jede Schlüssellänge");
    m.update(label);
    m.update(na);
    m.update(np);
    m
}

pub fn pc_tag(key: &[u8], na: &[u8], np: &[u8]) -> [u8; TAG] {
    mac(key, b"USBCD1 pc", na, np)
        .finalize()
        .into_bytes()
        .into()
}

#[cfg(test)]
pub fn app_tag(key: &[u8], na: &[u8], np: &[u8]) -> [u8; TAG] {
    mac(key, b"USBCD1 app", na, np)
        .finalize()
        .into_bytes()
        .into()
}

fn app_tag_ok(key: &[u8], na: &[u8], np: &[u8], tag: &[u8]) -> bool {
    mac(key, b"USBCD1 app", na, np).verify_slice(tag).is_ok()
}

#[derive(Debug, PartialEq, Eq)]
pub enum AuthError {
    /// keine Antwort – die App ist (noch) nicht geöffnet
    NoApp,
    /// alte App ohne Anmeldung
    Outdated,
    /// die App kennt unser Geheimnis nicht (z. B. von Hand geöffnet) – neu starten hilft
    Rejected,
    /// Gegenstelle hat sich falsch ausgewiesen: fremde App am USBCD-Anschluss
    Impostor,
}

/// PC-Seite des Handshakes.
pub fn handshake(s: &mut (impl Read + Write), key: &[u8]) -> Result<(), AuthError> {
    let (t, na) = read_frame_max(s, 4096).map_err(|_| AuthError::NoApp)?;
    match t {
        T_CHALLENGE if na.len() == NONCE => {}
        T_HELLO => return Err(AuthError::Outdated),
        _ => return Err(AuthError::Impostor),
    }
    let np = random::<NONCE>();
    let mut p = Vec::with_capacity(NONCE + TAG);
    p.extend_from_slice(&np);
    p.extend_from_slice(&pc_tag(key, &na, &np));
    write_frame(s, T_AUTH, &p).map_err(|_| AuthError::NoApp)?;
    let (t, tag) = read_frame_max(s, 4096).map_err(|_| AuthError::Rejected)?;
    if t != T_AUTH_OK || !app_tag_ok(key, &na, &np, &tag) {
        return Err(AuthError::Impostor);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::read_frame;
    use std::io::Cursor;

    /// Spielt die App: liest AUTH aus `pc_out`, prüft und antwortet.
    fn app_reply(key: &[u8], na: &[u8], pc_out: &[u8]) -> Option<Vec<u8>> {
        let (t, p) = read_frame(&mut Cursor::new(pc_out)).ok()?;
        assert_eq!(t, T_AUTH);
        let (np, tag) = p.split_at(NONCE);
        if pc_tag(key, na, np) != tag {
            return None;
        }
        let mut out = Vec::new();
        write_frame(&mut out, T_AUTH_OK, &app_tag(key, na, np)).unwrap();
        Some(out)
    }

    /// Duplex-Attrappe: liest aus `input`, schreibt nach `output`; nach dem ersten
    /// Schreiben wird die Antwort der „App“ angehängt.
    struct Duplex<'a> {
        input: Vec<u8>,
        pos: usize,
        output: Vec<u8>,
        app_key: &'a [u8],
        na: [u8; NONCE],
    }
    impl Read for Duplex<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = (self.input.len() - self.pos).min(buf.len());
            buf[..n].copy_from_slice(&self.input[self.pos..self.pos + n]);
            self.pos += n;
            Ok(n)
        }
    }
    impl Write for Duplex<'_> {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.output.extend_from_slice(b);
            if let Some(r) = app_reply(self.app_key, &self.na, &self.output) {
                self.input.extend_from_slice(&r);
            }
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn run(pc_key: &[u8], app_key: &[u8]) -> Result<(), AuthError> {
        let na = random::<NONCE>();
        let mut input = Vec::new();
        write_frame(&mut input, T_CHALLENGE, &na).unwrap();
        let mut d = Duplex {
            input,
            pos: 0,
            output: Vec::new(),
            app_key,
            na,
        };
        handshake(&mut d, pc_key)
    }

    #[test]
    fn same_key_succeeds() {
        assert_eq!(run(&[7; 32], &[7; 32]), Ok(()));
    }

    #[test]
    fn wrong_key_is_rejected() {
        assert_eq!(run(&[7; 32], &[8; 32]), Err(AuthError::Rejected));
    }

    #[test]
    fn old_app_detected() {
        let mut input = Vec::new();
        write_frame(&mut input, T_HELLO, b"{}").unwrap();
        let mut c = Cursor::new(input);
        let mut sink = Vec::new();
        struct Rw<'a>(&'a mut Cursor<Vec<u8>>, &'a mut Vec<u8>);
        impl Read for Rw<'_> {
            fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
                self.0.read(b)
            }
        }
        impl Write for Rw<'_> {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.1.write(b)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        assert_eq!(
            handshake(&mut Rw(&mut c, &mut sink), &[1; 32]),
            Err(AuthError::Outdated)
        );
    }

    #[test]
    fn known_vector() {
        // gleiche Werte prüft die Android-App (AuthTest in den Kommentaren von Auth.kt)
        let t = pc_tag(&[1; 32], &[2; 32], &[3; 32]);
        assert_eq!(t.len(), 32);
        assert_ne!(t, app_tag(&[1; 32], &[2; 32], &[3; 32]));
        println!("pc_tag={}", hex(&t));
    }
}

#[cfg(test)]
mod interop {
    /// Gegenprobe mit der Kotlin-Implementierung (manuell: Kotlin-Server auf Port 39999).
    #[test]
    #[ignore]
    fn against_kotlin() {
        use std::net::TcpStream;
        let mut s = TcpStream::connect(("127.0.0.1", 39999)).unwrap();
        assert_eq!(super::handshake(&mut s, &[5; 32]), Ok(()));
        let (t, p) = crate::proto::read_frame(&mut s).unwrap();
        assert_eq!((t, p.as_slice()), (1, &b"{\"width\":2000}"[..]));
        let mut s = TcpStream::connect(("127.0.0.1", 39999)).unwrap();
        assert_eq!(
            super::handshake(&mut s, &[6; 32]),
            Err(super::AuthError::Rejected)
        );
    }
}
