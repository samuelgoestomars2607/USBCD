//! Updates über GitHub-Releases – nur mit der Signatur des Projektinhabers.
//!
//! Ablauf: kurz nach dem Start und danach täglich fragt USBCD bei GitHub nach dem neuesten
//! Release (ohne Pre-releases). Ist es neuer, erscheint eine Benachrichtigung („Jetzt
//! aktualisieren“ / „Diese Version überspringen“) und im Menü „Update auf … installieren“.
//!
//! Sicherheit:
//! * Zu jedem Setup gehört eine Signaturdatei `USBCD-Setup-<v>.exe.sig`. Sie enthält Version,
//!   Dateiname und SHA-256 des Setups, unterschrieben (ECDSA P-256) mit dem Update-Schlüssel,
//!   den nur der Projektinhaber besitzt. Der öffentliche Teil steht in
//!   `assets/update-key.pub` und ist hier fest eingebaut. Ein Setup ohne gültige Unterschrift
//!   wird nie gestartet – auch nicht, wenn es aus dem echten GitHub-Projekt kommt.
//! * Die Prüfung macht Windows selbst (CNG, bcrypt.dll); die Verbindung läuft nur über HTTPS
//!   mit der Zertifikatsprüfung von Windows (WinHTTP).
//! * Nur Versionen, die neuer sind als die installierte (Version ist Teil der Unterschrift).
//! * Das Setup bleibt zwischen Prüfung und Start gesperrt (kein Austausch möglich).
//! * Alles läuft ohne Administratorrechte; die fragt erst das Setup selbst (Windows-Abfrage).
//! * Gesendet wird nichts außer der Anfrage selbst (GitHub sieht nur die IP-Adresse).

use crate::i18n::tr;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const API_URL: &str = "https://api.github.com/repos/samuelgoestomars2607/USBCD/releases/latest";
const DOWNLOAD_PREFIX: &str = "https://github.com/samuelgoestomars2607/USBCD/releases/download/";
const PAGE_PREFIX: &str = "https://github.com/samuelgoestomars2607/USBCD/releases/";
const PUBLIC_KEY: &str = include_str!("../assets/update-key.pub");

const LIMIT_JSON: usize = 1 << 20;
const LIMIT_SIG: usize = 4096;
const LIMIT_SETUP: usize = 400 << 20;

/// Erst so lange nach dem Start nachsehen (der Start soll nichts verzögern).
const FIRST_DELAY: Duration = Duration::from_secs(90);
const INTERVAL: Duration = Duration::from_secs(24 * 3600);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    setup_url: String,
    sig_url: String,
    pub page: String,
}

static AVAILABLE: Mutex<Option<Release>> = Mutex::new(None);
static NOTIFIED: Mutex<String> = Mutex::new(String::new());
static CHECK_NOW: AtomicBool = AtomicBool::new(false);
static BUSY: AtomicBool = AtomicBool::new(false);
static TEST: AtomicBool = AtomicBool::new(false);

// ------------------------------------------------------------ Versionen

/// "1.2.3" → (1, 2, 3); nur genau drei Zahlen.
pub fn parse_version(s: &str) -> Option<(u32, u32, u32)> {
    let mut it = s.split('.');
    let mut n = || -> Option<u32> {
        let p = it.next()?;
        if p.is_empty() || p.len() > 6 || !p.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        p.parse().ok()
    };
    let v = (n()?, n()?, n()?);
    if it.next().is_some() {
        return None;
    }
    Some(v)
}

pub fn valid_version(s: &str) -> bool {
    parse_version(s).is_some()
}

pub fn is_newer(candidate: &str, installed: &str) -> bool {
    match (parse_version(candidate), parse_version(installed)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

/// Installierte Version (bei `--update-test` 0.0.0, damit jedes Release als neuer gilt).
pub fn installed() -> &'static str {
    if TEST.load(Ordering::Relaxed) {
        "0.0.0"
    } else {
        env!("CARGO_PKG_VERSION")
    }
}

fn setup_name(v: &str) -> String {
    format!("USBCD-Setup-{v}.exe")
}

// ------------------------------------------------------------ Schlüssel und Signatur

fn unhex<const N: usize>(s: &str) -> Option<[u8; N]> {
    let b = s.as_bytes();
    if b.len() != N * 2 {
        return None;
    }
    let val = |c: u8| -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            _ => None,
        }
    };
    let mut out = [0u8; N];
    for (i, o) in out.iter_mut().enumerate() {
        *o = val(b[2 * i])? << 4 | val(b[2 * i + 1])?;
    }
    Some(out)
}

/// Öffentlicher Update-Schlüssel (X‖Y, je 32 Byte). None = nicht eingerichtet → keine Updates.
pub fn public_key() -> Option<[u8; 64]> {
    unhex::<64>(PUBLIC_KEY.trim())
}

/// Inhalt einer Signaturdatei.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signed {
    pub version: String,
    pub file: String,
    pub sha256: [u8; 32],
    pub signature: [u8; 64],
}

impl Signed {
    /// Genau dieser Text wird unterschrieben (siehe build-installer.ps1).
    pub fn message(&self) -> String {
        format!(
            "usbcd-update 1\nversion {}\nfile {}\nsha256 {}\n",
            self.version,
            self.file,
            crate::auth::hex(&self.sha256)
        )
    }
}

/// Liest eine Signaturdatei streng: fünf Zeilen in fester Reihenfolge.
pub fn parse_signed(text: &str) -> Option<Signed> {
    let lines: Vec<&str> = text
        .lines()
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| !l.is_empty())
        .collect();
    if lines.len() != 5 || lines[0] != "usbcd-update 1" {
        return None;
    }
    let version = lines[1].strip_prefix("version ")?;
    let file = lines[2].strip_prefix("file ")?;
    if !valid_version(version) || file != setup_name(version) {
        return None;
    }
    Some(Signed {
        version: version.to_string(),
        file: file.to_string(),
        sha256: unhex::<32>(lines[3].strip_prefix("sha256 ")?)?,
        signature: unhex::<64>(lines[4].strip_prefix("signature ")?)?,
    })
}

/// ECDSA P-256 / SHA-256 über die Nachricht prüfen – mit Windows-Kryptografie (CNG).
#[cfg(windows)]
fn verify_ecdsa(key: &[u8; 64], message: &[u8], signature: &[u8; 64]) -> bool {
    use windows_sys::Win32::Security::Cryptography::{
        BCryptCloseAlgorithmProvider, BCryptDestroyKey, BCryptImportKeyPair,
        BCryptOpenAlgorithmProvider, BCryptVerifySignature, BCRYPT_ALG_HANDLE,
        BCRYPT_ECCPUBLIC_BLOB, BCRYPT_ECDSA_P256_ALGORITHM, BCRYPT_ECDSA_PUBLIC_P256_MAGIC,
        BCRYPT_KEY_HANDLE,
    };
    let hash: [u8; 32] = Sha256::digest(message).into();
    // BCRYPT_ECCPUBLIC_BLOB: Kopf (Magic, Schlüssellänge) + X + Y
    let mut blob = Vec::with_capacity(8 + 64);
    blob.extend_from_slice(&BCRYPT_ECDSA_PUBLIC_P256_MAGIC.to_le_bytes());
    blob.extend_from_slice(&32u32.to_le_bytes());
    blob.extend_from_slice(key);
    unsafe {
        let mut alg: BCRYPT_ALG_HANDLE = std::ptr::null_mut();
        if BCryptOpenAlgorithmProvider(&mut alg, BCRYPT_ECDSA_P256_ALGORITHM, std::ptr::null(), 0)
            != 0
        {
            return false;
        }
        let mut h: BCRYPT_KEY_HANDLE = std::ptr::null_mut();
        let ok = BCryptImportKeyPair(
            alg,
            std::ptr::null_mut(),
            BCRYPT_ECCPUBLIC_BLOB,
            &mut h,
            blob.as_ptr(),
            blob.len() as u32,
            0,
        ) == 0
            && BCryptVerifySignature(
                h,
                std::ptr::null(),
                hash.as_ptr(),
                hash.len() as u32,
                signature.as_ptr(),
                signature.len() as u32,
                0,
            ) == 0;
        if !h.is_null() {
            BCryptDestroyKey(h);
        }
        BCryptCloseAlgorithmProvider(alg, 0);
        ok
    }
}

#[cfg(not(windows))]
fn verify_ecdsa(_key: &[u8; 64], _message: &[u8], _signature: &[u8; 64]) -> bool {
    false
}

/// Setup gesperrt öffnen: andere dürfen nur lesen – nicht schreiben, umbenennen oder löschen.
#[cfg(windows)]
fn open_locked(p: &Path) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 1;
    std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(p)
}

#[cfg(not(windows))]
fn open_locked(p: &Path) -> std::io::Result<std::fs::File> {
    std::fs::File::open(p)
}

/// Prüft Setup und Signatur. Liefert die gesperrt geöffnete Datei – solange sie offen
/// bleibt, kann niemand das geprüfte Setup austauschen.
pub fn verify_setup(setup: &Path, sig_text: &str) -> anyhow::Result<(Signed, std::fs::File)> {
    let key = public_key().ok_or_else(|| anyhow::anyhow!("kein Update-Schlüssel eingebaut"))?;
    let signed = parse_signed(sig_text).ok_or_else(|| anyhow::anyhow!("Signaturdatei ungültig"))?;
    let name = setup.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if name != signed.file {
        anyhow::bail!("Signatur gehört zu {}, nicht zu {name}", signed.file);
    }
    let mut f = open_locked(setup)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    let digest: [u8; 32] = h.finalize().into();
    if digest != signed.sha256 {
        anyhow::bail!("Prüfsumme des Setups passt nicht zur Signatur");
    }
    if !verify_ecdsa(&key, signed.message().as_bytes(), &signed.signature) {
        anyhow::bail!("Unterschrift ungültig – nicht vom USBCD-Herausgeber");
    }
    Ok((signed, f))
}

/// `USBCD.exe --verify-update <setup> <sig>` (Selbsttest im Build): 0 = gültig.
pub fn verify_cli(setup: &str, sig: &str) -> i32 {
    let text = match std::fs::read(sig) {
        Ok(b) if b.len() <= LIMIT_SIG => String::from_utf8_lossy(&b).into_owned(),
        _ => return 2,
    };
    match verify_setup(Path::new(setup), &text) {
        Ok(_) => 0,
        Err(_) => 1,
    }
}

// ------------------------------------------------------------ GitHub-Antwort

#[derive(serde::Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
}

#[derive(serde::Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

/// Liest „neuestes Release“. Nur reguläre Releases mit Tag `v<x.y.z>`, Setup und
/// Signatur genau unter der erwarteten Adresse dieses Projekts.
pub fn parse_release(json: &str) -> Option<Release> {
    let r: GhRelease = serde_json::from_str(json).ok()?;
    if r.draft || r.prerelease {
        return None;
    }
    let version = r.tag_name.strip_prefix('v')?;
    if !valid_version(version) {
        return None;
    }
    let name = setup_name(version);
    let base = format!("{DOWNLOAD_PREFIX}v{version}/");
    let url_of = |n: &str| -> Option<String> {
        let a = r.assets.iter().find(|a| a.name == n)?;
        (a.browser_download_url == format!("{base}{n}")).then(|| a.browser_download_url.clone())
    };
    let setup_url = url_of(&name)?;
    let sig_url = url_of(&format!("{name}.sig"))?;
    let page = if r.html_url.starts_with(PAGE_PREFIX) {
        r.html_url
    } else {
        format!("{PAGE_PREFIX}tag/v{version}")
    };
    Some(Release {
        version: version.to_string(),
        setup_url,
        sig_url,
        page,
    })
}

// ------------------------------------------------------------ HTTPS (WinHTTP)

#[cfg_attr(not(windows), allow(dead_code))]
enum HttpError {
    NotFound,
    Other(anyhow::Error),
}

impl From<anyhow::Error> for HttpError {
    fn from(e: anyhow::Error) -> Self {
        HttpError::Other(e)
    }
}

impl From<std::io::Error> for HttpError {
    fn from(e: std::io::Error) -> Self {
        HttpError::Other(e.into())
    }
}

#[cfg(windows)]
struct Handle(*mut core::ffi::c_void);

#[cfg(windows)]
impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { windows_sys::Win32::Networking::WinHttp::WinHttpCloseHandle(self.0) };
        }
    }
}

/// GET über HTTPS; schreibt den Inhalt in `sink`, höchstens `limit` Bytes.
#[cfg(windows)]
fn https_get(
    url: &str,
    accept: &str,
    limit: usize,
    sink: &mut dyn FnMut(&[u8]) -> std::io::Result<()>,
) -> Result<(), HttpError> {
    use windows_sys::Win32::Networking::WinHttp::{
        WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders, WinHttpReadData,
        WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetTimeouts,
        INTERNET_DEFAULT_HTTPS_PORT, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE,
        WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
    };
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| anyhow::anyhow!("nur HTTPS erlaubt"))?;
    let (host, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    if host.is_empty()
        || !host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-.".contains(&b))
    {
        return Err(anyhow::anyhow!("ungültige Adresse").into());
    }
    let w = crate::win::wide;
    let agent = w(&format!("USBCD/{}", env!("CARGO_PKG_VERSION")));
    unsafe {
        let session = Handle(WinHttpOpen(
            agent.as_ptr(),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            std::ptr::null(),
            std::ptr::null(),
            0,
        ));
        if session.0.is_null() {
            return Err(anyhow::anyhow!("WinHTTP nicht verfügbar").into());
        }
        WinHttpSetTimeouts(session.0, 15_000, 15_000, 30_000, 60_000);
        let whost = w(host);
        let conn = Handle(WinHttpConnect(
            session.0,
            whost.as_ptr(),
            INTERNET_DEFAULT_HTTPS_PORT,
            0,
        ));
        if conn.0.is_null() {
            return Err(std::io::Error::last_os_error().into());
        }
        let verb = w("GET");
        let wpath = w(path);
        // WINHTTP_FLAG_SECURE: TLS mit Zertifikatsprüfung durch Windows. Weiterleitungen
        // (GitHub → Download-Server) folgt WinHTTP selbst – nie von HTTPS zu HTTP.
        let req = Handle(WinHttpOpenRequest(
            conn.0,
            verb.as_ptr(),
            wpath.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        ));
        if req.0.is_null() {
            return Err(std::io::Error::last_os_error().into());
        }
        let headers = w(&format!("Accept: {accept}\r\n"));
        if WinHttpSendRequest(
            req.0,
            headers.as_ptr(),
            (headers.len() - 1) as u32,
            std::ptr::null(),
            0,
            0,
            0,
        ) == 0
            || WinHttpReceiveResponse(req.0, std::ptr::null_mut()) == 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        let mut status: u32 = 0;
        let mut len = std::mem::size_of::<u32>() as u32;
        if WinHttpQueryHeaders(
            req.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            std::ptr::null(),
            &mut status as *mut u32 as *mut core::ffi::c_void,
            &mut len,
            std::ptr::null_mut(),
        ) == 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        if status == 404 {
            return Err(HttpError::NotFound);
        }
        if status != 200 {
            return Err(anyhow::anyhow!("HTTP {status}").into());
        }
        let mut buf = vec![0u8; 1 << 16];
        let mut total = 0usize;
        loop {
            let mut n: u32 = 0;
            if WinHttpReadData(
                req.0,
                buf.as_mut_ptr() as *mut core::ffi::c_void,
                buf.len() as u32,
                &mut n,
            ) == 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
            if n == 0 {
                break;
            }
            total += n as usize;
            if total > limit {
                return Err(anyhow::anyhow!("Antwort zu groß").into());
            }
            sink(&buf[..n as usize])?;
        }
        Ok(())
    }
}

#[cfg(not(windows))]
fn https_get(
    _url: &str,
    _accept: &str,
    _limit: usize,
    _sink: &mut dyn FnMut(&[u8]) -> std::io::Result<()>,
) -> Result<(), HttpError> {
    Err(anyhow::anyhow!("nur unter Windows").into())
}

fn get_bytes(url: &str, accept: &str, limit: usize) -> Result<Vec<u8>, HttpError> {
    let mut v = Vec::new();
    https_get(url, accept, limit, &mut |b| {
        v.extend_from_slice(b);
        Ok(())
    })?;
    Ok(v)
}

/// Neuestes reguläres Release; None = keins vorhanden (oder ohne gültige Dateien).
fn fetch_latest() -> anyhow::Result<Option<Release>> {
    match get_bytes(API_URL, "application/vnd.github+json", LIMIT_JSON) {
        Ok(b) => Ok(parse_release(&String::from_utf8_lossy(&b))),
        Err(HttpError::NotFound) => Ok(None),
        Err(HttpError::Other(e)) => Err(e),
    }
}

// ------------------------------------------------------------ Prüfen im Hintergrund

/// Gefundenes neueres Release (für das Menü).
pub fn available() -> Option<Release> {
    AVAILABLE.lock().ok().and_then(|g| g.clone())
}

/// Menü „Automatisch nach Updates suchen“ eingeschaltet → gleich nachsehen.
pub fn check_now() {
    CHECK_NOW.store(true, Ordering::SeqCst);
}

/// Automatische Suche ausgeschaltet → Hinweis im Menü entfernen.
pub fn clear() {
    if let Ok(mut g) = AVAILABLE.lock() {
        *g = None;
    }
}

/// Startet die Update-Prüfung im Hintergrund (`test`: --update-test).
pub fn start(test: bool) {
    TEST.store(test, Ordering::Relaxed);
    if public_key().is_none() {
        crate::log!("Updates: kein Update-Schlüssel eingebaut – automatische Updates aus");
        return;
    }
    if test {
        crate::log!("Updates: Testmodus – jedes Release gilt als neuer");
        CHECK_NOW.store(true, Ordering::SeqCst);
    }
    std::thread::spawn(|| {
        let mut wait = FIRST_DELAY;
        loop {
            let t = Instant::now();
            while t.elapsed() < wait && !CHECK_NOW.load(Ordering::SeqCst) {
                if crate::state::shutdown() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(500));
            }
            if crate::state::shutdown() {
                return;
            }
            let forced = CHECK_NOW.swap(false, Ordering::SeqCst);
            if forced || crate::config::current().update_check {
                check();
            }
            wait = INTERVAL;
        }
    });
}

fn check() {
    let release = match fetch_latest() {
        Ok(r) => r,
        Err(e) => {
            crate::log!("Updates: Abfrage nicht möglich ({e:#})");
            return;
        }
    };
    let newer = release.filter(|r| is_newer(&r.version, installed()));
    if let Ok(mut g) = AVAILABLE.lock() {
        *g = newer.clone();
    }
    let Some(r) = newer else {
        crate::log!("Updates: USBCD {} ist aktuell", installed());
        return;
    };
    crate::log!("Updates: Version {} verfügbar ({})", r.version, r.page);
    let skipped = crate::config::current().update_skip == r.version;
    let already = NOTIFIED.lock().map(|g| *g == r.version).unwrap_or(false);
    if skipped || already {
        return;
    }
    if let Ok(mut g) = NOTIFIED.lock() {
        *g = r.version.clone();
    }
    let v = &r.version;
    crate::notify::show(crate::notify::Toast {
        title: tr("USBCD-Update verfügbar", "USBCD update available").to_string(),
        text: tr(
            "Version {v} ist erschienen (installiert: {i}).",
            "Version {v} is available (installed: {i}).",
        )
        .replace("{v}", v)
        .replace("{i}", installed()),
        actions: vec![
            (
                tr("Jetzt aktualisieren", "Update now").to_string(),
                format!("update?act=install&v={v}"),
            ),
            (
                tr("Diese Version überspringen", "Skip this version").to_string(),
                format!("update?act=skip&v={v}"),
            ),
        ],
        silent: false,
        long: true,
    });
}

// ------------------------------------------------------------ Installieren

fn update_dir() -> PathBuf {
    crate::config::data_dir().join("update")
}

/// Menü „Update installieren“ bzw. Knopf in der Benachrichtigung.
pub fn install(version: &str) {
    let Some(r) = available() else {
        crate::log!("Updates: kein Update bekannt – Klick ignoriert");
        return;
    };
    if r.version != version {
        crate::log!(
            "Updates: Klick für {version}, verfügbar ist {} – ignoriert",
            r.version
        );
        return;
    }
    if BUSY.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(move || {
        if let Err(e) = download_and_run(&r) {
            crate::log!("Updates: {e:#}");
        }
        BUSY.store(false, Ordering::SeqCst);
    });
}

fn fail(text: &str, security: bool) {
    crate::notify::once(
        if security { "security" } else { "updatefail" },
        tr(
            "USBCD-Update nicht installiert",
            "USBCD update not installed",
        ),
        text,
        Duration::ZERO,
    );
}

fn download_and_run(r: &Release) -> anyhow::Result<()> {
    let dir = update_dir();
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    crate::notify::once(
        "updating",
        tr("USBCD-Update wird geladen", "Downloading USBCD update"),
        &tr(
            "Version {v} wird heruntergeladen und geprüft …",
            "Version {v} is being downloaded and verified …",
        )
        .replace("{v}", &r.version),
        Duration::ZERO,
    );

    let sig = match get_bytes(&r.sig_url, "application/octet-stream", LIMIT_SIG) {
        Ok(b) => String::from_utf8_lossy(&b).into_owned(),
        Err(_) => {
            fail(
                tr(
                    "Die Signatur konnte nicht geladen werden. Bitte später erneut versuchen.",
                    "The signature could not be downloaded. Please try again later.",
                ),
                false,
            );
            anyhow::bail!("Signatur nicht geladen");
        }
    };
    let setup = dir.join(setup_name(&r.version));
    let loaded = (|| -> Result<(), HttpError> {
        let mut f = std::fs::File::create(&setup)?;
        https_get(
            &r.setup_url,
            "application/octet-stream",
            LIMIT_SETUP,
            &mut |b| f.write_all(b),
        )?;
        f.sync_all()?;
        Ok(())
    })();
    if loaded.is_err() {
        let _ = std::fs::remove_dir_all(&dir);
        fail(
            tr(
                "Das Setup konnte nicht heruntergeladen werden. Bitte später erneut versuchen.",
                "The setup could not be downloaded. Please try again later.",
            ),
            false,
        );
        anyhow::bail!("Setup nicht geladen");
    }

    let (signed, locked) = match verify_setup(&setup, &sig) {
        Ok(v) => v,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dir);
            crate::log!("Updates: Setup abgelehnt: {e:#}");
            fail(
                tr(
                    "Das heruntergeladene Setup trägt keine gültige Unterschrift des USBCD-Herausgebers und wurde gelöscht. Bitte melde das im USBCD-Projekt auf GitHub.",
                    "The downloaded setup has no valid signature from the USBCD publisher and was deleted. Please report this in the USBCD project on GitHub.",
                ),
                true,
            );
            anyhow::bail!("Signaturprüfung fehlgeschlagen");
        }
    };
    if signed.version != r.version || !is_newer(&signed.version, installed()) {
        drop(locked);
        let _ = std::fs::remove_dir_all(&dir);
        fail(
            tr(
                "Das Setup ist nicht neuer als die installierte Version.",
                "The setup is not newer than the installed version.",
            ),
            true,
        );
        anyhow::bail!("Version {} nicht neuer", signed.version);
    }
    crate::log!(
        "Updates: Setup {} geprüft – Unterschrift gültig",
        signed.version
    );

    match run_setup(&setup) {
        Ok(()) => {
            crate::log!("Updates: Setup gestartet – USBCD wird beendet");
            // Datei bleibt bis zum Programmende gesperrt (das Setup beendet USBCD ohnehin)
            std::mem::forget(locked);
            crate::tray::request_quit();
            Ok(())
        }
        Err(cancelled) => {
            drop(locked);
            let _ = std::fs::remove_dir_all(&dir);
            if cancelled {
                crate::log!("Updates: Windows-Abfrage abgelehnt");
            } else {
                fail(
                    tr(
                        "Das Setup konnte nicht gestartet werden.",
                        "The setup could not be started.",
                    ),
                    false,
                );
            }
            Ok(())
        }
    }
}

/// Startet das geprüfte Setup mit Administratorrechten (Windows fragt nach).
/// Err(true) = Abfrage abgelehnt.
#[cfg(windows)]
fn run_setup(setup: &Path) -> Result<(), bool> {
    use windows_sys::Win32::UI::Shell::{
        ShellExecuteExW, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW,
    };
    let verb = crate::win::wide("runas");
    let file = crate::win::wide(&setup.to_string_lossy());
    // /SILENT: nur Fortschritt; Aufgaben und Sprache übernimmt Inno von der letzten Installation
    let params = crate::win::wide("/SILENT /SP-");
    unsafe {
        let mut info: SHELLEXECUTEINFOW = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
        info.fMask = SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI;
        info.lpVerb = verb.as_ptr();
        info.lpFile = file.as_ptr();
        info.lpParameters = params.as_ptr();
        info.nShow = windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        if ShellExecuteExW(&mut info) != 0 {
            Ok(())
        } else {
            const ERROR_CANCELLED: i32 = 1223;
            Err(std::io::Error::last_os_error().raw_os_error() == Some(ERROR_CANCELLED))
        }
    }
}

#[cfg(not(windows))]
fn run_setup(_setup: &Path) -> Result<(), bool> {
    Err(false)
}

/// Reste eines früheren Updates entfernen (beim Start).
pub fn cleanup() {
    let _ = std::fs::remove_dir_all(update_dir());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(parse_version("0.6.15"), Some((0, 6, 15)));
        assert!(parse_version("0.6").is_none());
        assert!(parse_version("0.6.15.1").is_none());
        assert!(parse_version("0.6.x").is_none());
        assert!(parse_version("-1.0.0").is_none());
        assert!(parse_version("0.6.15 ").is_none());
        assert!(is_newer("0.6.15", "0.6.14"));
        assert!(is_newer("0.10.0", "0.9.99"));
        assert!(!is_newer("0.6.14", "0.6.14"));
        assert!(!is_newer("0.6.13", "0.6.14"));
        assert!(!is_newer("x", "0.6.14"));
    }

    fn sample_json(tag: &str, pre: bool, url_prefix: &str) -> String {
        let v = tag.trim_start_matches('v');
        format!(
            r#"{{"tag_name":"{tag}","draft":false,"prerelease":{pre},
            "html_url":"https://github.com/samuelgoestomars2607/USBCD/releases/tag/{tag}",
            "assets":[
              {{"name":"USBCD-Setup-{v}.exe","browser_download_url":"{url_prefix}{tag}/USBCD-Setup-{v}.exe"}},
              {{"name":"USBCD-Setup-{v}.exe.sig","browser_download_url":"{url_prefix}{tag}/USBCD-Setup-{v}.exe.sig"}}
            ]}}"#
        )
    }

    #[test]
    fn release_parsing() {
        let r = parse_release(&sample_json("v0.6.15", false, DOWNLOAD_PREFIX)).unwrap();
        assert_eq!(r.version, "0.6.15");
        assert!(r.setup_url.ends_with("/v0.6.15/USBCD-Setup-0.6.15.exe"));
        assert!(r.sig_url.ends_with(".exe.sig"));
        // Pre-releases (z. B. build-deps) und fremde Adressen werden ignoriert
        assert!(parse_release(&sample_json("v0.6.15", true, DOWNLOAD_PREFIX)).is_none());
        assert!(parse_release(&sample_json(
            "v0.6.15",
            false,
            "https://github.com/someone/USBCD/releases/download/"
        ))
        .is_none());
        assert!(parse_release(&sample_json("build-deps-1", false, DOWNLOAD_PREFIX)).is_none());
        assert!(parse_release("kein json").is_none());
    }

    /// Weitere Fälle, die der Updater ablehnen muss.
    #[test]
    fn release_rejections() {
        let ok = sample_json("v0.6.15", false, DOWNLOAD_PREFIX);
        // Entwurf
        assert!(parse_release(&ok.replace("\"draft\":false", "\"draft\":true")).is_none());
        // unverschlüsselte Adresse
        assert!(parse_release(&sample_json(
            "v0.6.15",
            false,
            "http://github.com/samuelgoestomars2607/USBCD/releases/download/"
        ))
        .is_none());
        // Tag mit Pfadanteilen oder ohne „v“
        assert!(parse_release(&sample_json("v0.6.15/../x", false, DOWNLOAD_PREFIX)).is_none());
        assert!(parse_release(&sample_json("0.6.15", false, DOWNLOAD_PREFIX)).is_none());
        // Signatur fehlt
        let no_sig = ok.replace("USBCD-Setup-0.6.15.exe.sig\"", "andere.sig\"");
        assert!(parse_release(&no_sig).is_none());
        // fremde Release-Seite wird durch die eigene ersetzt
        let page = parse_release(&ok.replace(
            "https://github.com/samuelgoestomars2607/USBCD/releases/tag/v0.6.15",
            "https://example.com/x",
        ))
        .unwrap()
        .page;
        assert!(page.starts_with(PAGE_PREFIX));
    }

    /// Signaturdatei: genau fünf Zeilen, nichts davor, dazwischen oder danach.
    #[test]
    fn signature_file_rejections() {
        let sha = "ab".repeat(32);
        let sig = "cd".repeat(64);
        let text = format!(
            "usbcd-update 1\nversion 0.6.15\nfile USBCD-Setup-0.6.15.exe\nsha256 {sha}\nsignature {sig}\n"
        );
        assert!(parse_signed(&format!("{text}extra 1\n")).is_none());
        assert!(parse_signed(&text.replace(&format!("sha256 {sha}\n"), "")).is_none());
        assert!(parse_signed(&text.replace("version 0.6.15", "version 0.6.15-beta")).is_none());
        assert!(
            parse_signed(&text.replace("file USBCD-Setup-0.6.15.exe", "file ..\\x.exe")).is_none()
        );
        assert!(parse_signed(&text.replace("signature ", "signature  ")).is_none());
    }

    /// Der eingebaute öffentliche Schlüssel ist gültig – sonst gäbe es gar keine Updates.
    #[test]
    fn public_key_present() {
        assert!(public_key().is_some());
    }

    #[test]
    fn signature_file() {
        let sha = "ab".repeat(32);
        let sig = "cd".repeat(64);
        let text = format!(
            "usbcd-update 1\nversion 0.6.15\nfile USBCD-Setup-0.6.15.exe\nsha256 {sha}\nsignature {sig}\n"
        );
        let s = parse_signed(&text).unwrap();
        assert_eq!(s.version, "0.6.15");
        assert_eq!(
            s.message(),
            format!("usbcd-update 1\nversion 0.6.15\nfile USBCD-Setup-0.6.15.exe\nsha256 {sha}\n")
        );
        // Dateiname muss zur Version passen, Hex nur klein und vollständig
        assert!(
            parse_signed(&text.replace("file USBCD-Setup-0.6.15", "file USBCD-Setup-0.6.16"))
                .is_none()
        );
        assert!(parse_signed(&text.replace(&sha, &sha.to_uppercase())).is_none());
        assert!(parse_signed(&text.replace(&sig, &sig[2..])).is_none());
        assert!(parse_signed(&text.replace("usbcd-update 1", "usbcd-update 2")).is_none());
        assert!(parse_signed(&text.replace('\n', "\r\n")).is_some());
    }

    #[test]
    fn hex() {
        assert_eq!(unhex::<2>("0aff"), Some([0x0a, 0xff]));
        assert_eq!(unhex::<2>("0aFF"), None);
        assert_eq!(unhex::<2>("0af"), None);
    }
}
