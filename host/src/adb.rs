use crate::util::command;
use anyhow::{bail, Context, Result};

pub struct Adb {
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub serial: String,
    pub state: String,
}

impl Adb {
    pub fn run(&self, args: &[&str]) -> Result<String> {
        crate::util::may_spawn()?;
        let o = command(&self.path).args(args).output().with_context(|| {
            format!(
                "adb nicht startbar ({}) – Android Platform-Tools installiert?",
                self.path
            )
        })?;
        let out = String::from_utf8_lossy(&o.stdout).into_owned();
        if !o.status.success() {
            bail!(
                "adb {:?} fehlgeschlagen: {}{}",
                args,
                out,
                String::from_utf8_lossy(&o.stderr)
            );
        }
        Ok(out)
    }

    pub fn on(&self, serial: &str, args: &[&str]) -> Result<String> {
        let mut a = vec!["-s", serial];
        a.extend_from_slice(args);
        self.run(&a)
    }

    pub fn devices(&self) -> Result<Vec<Device>> {
        Ok(parse_devices(&self.run(&["devices"])?))
    }

    /// Shell-Befehl direkt über den laufenden adb-Dienst (Port 5037), ohne ein adb-Programm
    /// zu starten – funktioniert deshalb auch noch, wenn Windows herunterfährt.
    pub fn shell_direct(serial: &str, cmd: &str, timeout: std::time::Duration) -> Result<String> {
        use std::io::{Read, Write};
        let port = std::env::var("ANDROID_ADB_SERVER_PORT")
            .ok()
            .and_then(|p| p.parse::<u16>().ok())
            .unwrap_or(5037);
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
        let mut s = std::net::TcpStream::connect_timeout(&addr, timeout).context("adb-Dienst")?;
        s.set_read_timeout(Some(timeout))?;
        s.set_write_timeout(Some(timeout))?;
        let request = |s: &mut std::net::TcpStream, msg: &str| -> Result<()> {
            s.write_all(format!("{:04x}{msg}", msg.len()).as_bytes())?;
            let mut status = [0u8; 4];
            s.read_exact(&mut status)?;
            if &status != b"OKAY" {
                bail!("adb-Dienst lehnt ab: {}", String::from_utf8_lossy(&status));
            }
            Ok(())
        };
        request(&mut s, &format!("host:transport:{serial}"))?;
        request(&mut s, &format!("shell:{cmd}"))?;
        // Ausgabe bis zum Ende lesen: dann ist der Befehl auf dem Tablet fertig
        let mut out = Vec::new();
        let _ = s.read_to_end(&mut out);
        Ok(String::from_utf8_lossy(&out).into_owned())
    }
}

/// Beobachtet angeschlossene Geräte mit `adb track-devices`: Ein einziger, dauerhaft
/// laufender adb-Prozess meldet Änderungen – statt jede Sekunde `adb devices` zu starten
/// (das waren im Leerlauf rund 86 000 Prozessstarts am Tag).
pub struct Tracker {
    shared: std::sync::Arc<(std::sync::Mutex<TrackState>, std::sync::Condvar)>,
}

#[derive(Default)]
struct TrackState {
    list: Option<Vec<Device>>,
    generation: u64,
}

impl Tracker {
    pub fn start(adb: &Adb) -> Tracker {
        let shared = std::sync::Arc::new((
            std::sync::Mutex::new(TrackState::default()),
            std::sync::Condvar::new(),
        ));
        let s = shared.clone();
        let path = adb.path.clone();
        std::thread::Builder::new()
            .name("adb-track".into())
            .spawn(move || track_loop(&path, &s))
            .ok();
        Tracker { shared }
    }

    /// Wartet höchstens `timeout` auf eine Änderung und liefert die aktuelle Liste.
    /// `None`: Beobachtung läuft gerade nicht (dann `adb devices` verwenden).
    pub fn wait(&self, seen: &mut u64, timeout: std::time::Duration) -> Option<Vec<Device>> {
        let (m, cv) = &*self.shared;
        let g = m.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let (g, _) = cv
            .wait_timeout_while(g, timeout, |s| s.generation == *seen)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *seen = g.generation;
        g.list.clone()
    }
}

fn track_loop(path: &str, shared: &(std::sync::Mutex<TrackState>, std::sync::Condvar)) {
    use std::io::Read;
    let set = |list: Option<Vec<Device>>| {
        let mut g = shared
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if g.list != list {
            g.list = list;
            g.generation += 1;
            shared.1.notify_all();
        }
    };
    while !crate::util::shutdown() {
        if crate::util::windows_ending() {
            // beim Herunterfahren adb nicht neu starten (sonst Fehlerfenster 0xc0000142)
            std::thread::sleep(std::time::Duration::from_millis(500));
            continue;
        }
        // Server zuerst starten – er soll nicht an USBCD gebunden sein
        let _ = Adb {
            path: path.to_string(),
        }
        .run(&["start-server"]);
        let child = command(path)
            .arg("track-devices")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn();
        let Ok(mut child) = child else {
            std::thread::sleep(std::time::Duration::from_secs(5));
            continue;
        };
        crate::util::bind_to_us(&child);
        if let Some(mut out) = child.stdout.take() {
            loop {
                // Format: 4 Hex-Ziffern Länge, dann „serial<TAB>state“-Zeilen
                let mut len = [0u8; 4];
                if out.read_exact(&mut len).is_err() {
                    break;
                }
                let Some(n) = std::str::from_utf8(&len)
                    .ok()
                    .and_then(|s| usize::from_str_radix(s, 16).ok())
                else {
                    break;
                };
                let mut body = vec![0u8; n];
                if out.read_exact(&mut body).is_err() {
                    break;
                }
                set(Some(parse_track(&String::from_utf8_lossy(&body))));
            }
        }
        let _ = child.kill();
        let _ = child.wait();
        set(None);
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

/// Inhalt einer `track-devices`-Meldung (ohne Kopfzeile).
pub fn parse_track(body: &str) -> Vec<Device> {
    parse_devices(&format!("List of devices attached\n{body}"))
}

pub fn parse_devices(out: &str) -> Vec<Device> {
    out.lines()
        .skip_while(|l| !l.starts_with("List of devices"))
        .skip(1)
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let serial = it.next()?.to_string();
            let state = it.next()?.to_string();
            // WLAN-ADB ignorieren – nur USB-Geräte
            if serial.contains(':') || serial.starts_with("emulator-") {
                return None;
            }
            Some(Device { serial, state })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn track_body() {
        let d = super::parse_track("TAB0123456X\tdevice\nemulator-5554\tdevice\n");
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].serial, "TAB0123456X");
        assert!(super::parse_track("").is_empty());
    }

    #[test]
    fn parse() {
        let d = super::parse_devices(
            "* daemon started\nList of devices attached\nR52T1234\tdevice\n192.168.0.5:5555\tdevice\nABC\tunauthorized\n\n",
        );
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].serial, "R52T1234");
        assert_eq!(d[1].state, "unauthorized");
    }
}
