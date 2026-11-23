//! Zerlegt einen H.264/H.265-Annex-B-Bytestrom (so wie ffmpeg ihn auf stdout
//! schreibt) in vollständige Access Units (= ein Bild pro Einheit).
//! MediaCodec auf dem Tablet erwartet genau ein Bild pro Eingabepuffer.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    Avc,
    Hevc,
}

impl Codec {
    pub fn name(self) -> &'static str {
        match self {
            Codec::Avc => "avc",
            Codec::Hevc => "hevc",
        }
    }
}

/// Ein vollständiges Bild.
#[derive(Debug, Default, Clone)]
pub struct AccessUnit {
    /// Parameter-Sets (VPS/SPS/PPS) dieses Bildes, falls vorhanden – werden
    /// dem Decoder als CODEC_CONFIG-Puffer übergeben.
    pub config: Vec<u8>,
    /// Restliche NAL-Einheiten mit 4-Byte-Startcodes.
    pub data: Vec<u8>,
    /// Enthält ein IDR/IRAP-Bild.
    pub key: bool,
}

const START: [u8; 4] = [0, 0, 0, 1];

struct NalInfo {
    vcl: bool,
    first_slice: bool,
    starts_au: bool,
    param_set: bool,
    key: bool,
}

fn classify(codec: Codec, nal: &[u8]) -> NalInfo {
    match codec {
        Codec::Avc => {
            let t = nal[0] & 0x1f;
            let vcl = (1..=5).contains(&t);
            NalInfo {
                vcl,
                first_slice: vcl && nal.len() > 1 && nal[1] & 0x80 != 0,
                starts_au: matches!(t, 6..=9 | 14..=18),
                param_set: matches!(t, 7 | 8),
                key: t == 5,
            }
        }
        Codec::Hevc => {
            let t = (nal[0] >> 1) & 0x3f;
            let vcl = t <= 31;
            NalInfo {
                vcl,
                first_slice: vcl && nal.len() > 2 && nal[2] & 0x80 != 0,
                starts_au: matches!(t, 32..=35 | 39 | 41..=44 | 48..=55),
                param_set: matches!(t, 32..=34),
                key: (16..=21).contains(&t),
            }
        }
    }
}

/// Sucht ab `from` den nächsten Startcode. Gibt (Position, Länge 3 oder 4) zurück.
fn find_start(buf: &[u8], from: usize) -> Option<(usize, usize)> {
    let mut i = from;
    while i + 3 <= buf.len() {
        if buf[i + 2] > 1 {
            i += 3;
            continue;
        }
        if buf[i] == 0 && buf[i + 1] == 0 && buf[i + 2] == 1 {
            if i > from && buf[i - 1] == 0 {
                return Some((i - 1, 4));
            }
            return Some((i, 3));
        }
        i += 1;
    }
    None
}

pub struct AnnexBSplitter {
    codec: Codec,
    /// beginnt immer mit einem Startcode (Länge `sc_len`), sofern `sc_len > 0`
    buf: Vec<u8>,
    sc_len: usize,
    /// Kopf der gerade einlaufenden NAL-Einheit schon ausgewertet?
    header_done: bool,
    cur: AccessUnit,
    cur_has_vcl: bool,
}

impl AnnexBSplitter {
    pub fn new(codec: Codec) -> Self {
        Self {
            codec,
            buf: Vec::with_capacity(1 << 20),
            sc_len: 0,
            header_done: false,
            cur: AccessUnit::default(),
            cur_has_vcl: false,
        }
    }

    /// Neue Bytes einspeisen; fertige Bilder landen in `out`.
    /// Ein Bild gilt als fertig, sobald der Kopf der ersten NAL-Einheit des
    /// nächsten Bildes eintrifft – oder früher über `flush_pending`.
    pub fn push(&mut self, bytes: &[u8], out: &mut Vec<AccessUnit>) {
        self.buf.extend_from_slice(bytes);
        if self.sc_len == 0 {
            match find_start(&self.buf, 0) {
                Some((s, l)) => {
                    self.buf.drain(..s);
                    self.sc_len = l;
                    self.header_done = false;
                }
                None => {
                    // Bruchstück ohne Startcode verwerfen (bis auf mögliche Startcode-Reste)
                    let keep = self.buf.len().min(3);
                    let cut = self.buf.len() - keep;
                    self.buf.drain(..cut);
                    return;
                }
            }
        }
        loop {
            if !self.header_done {
                if self.buf.len() < self.sc_len + 3 {
                    return;
                }
                let hdr = [
                    self.buf[self.sc_len],
                    self.buf[self.sc_len + 1],
                    self.buf[self.sc_len + 2],
                ];
                self.check_boundary(&hdr, out);
                self.header_done = true;
            }
            match find_start(&self.buf, self.sc_len) {
                Some((n, nl)) => {
                    let nal = self.buf[self.sc_len..n].to_vec();
                    self.add_nal(&nal);
                    self.buf.drain(..n);
                    self.sc_len = nl;
                    self.header_done = false;
                }
                None => return,
            }
        }
    }

    /// Die zuletzt eingelaufene NAL-Einheit als vollständig betrachten und das
    /// aktuelle Bild sofort ausgeben (wenn ffmpeg gerade nichts mehr schreibt
    /// oder am Stromende).
    pub fn flush_pending(&mut self, out: &mut Vec<AccessUnit>) {
        if self.sc_len > 0 && self.buf.len() > self.sc_len {
            let nal = self.buf[self.sc_len..].to_vec();
            if !self.header_done {
                let mut hdr = [0u8; 3];
                for (i, b) in nal.iter().take(3).enumerate() {
                    hdr[i] = *b;
                }
                self.check_boundary(&hdr, out);
            }
            self.add_nal(&nal);
            self.buf.clear();
            self.sc_len = 0;
            self.header_done = false;
        }
        if self.cur_has_vcl {
            out.push(std::mem::take(&mut self.cur));
            self.cur_has_vcl = false;
        }
    }

    fn check_boundary(&mut self, hdr: &[u8], out: &mut Vec<AccessUnit>) {
        let info = classify(self.codec, hdr);
        if self.cur_has_vcl && ((info.vcl && info.first_slice) || info.starts_au) {
            out.push(std::mem::take(&mut self.cur));
            self.cur_has_vcl = false;
        }
    }

    fn add_nal(&mut self, nal: &[u8]) {
        // abschließende Null-Bytes (trailing_zero_8bits) entfernen
        let mut end = nal.len();
        while end > 0 && nal[end - 1] == 0 {
            end -= 1;
        }
        let nal = &nal[..end];
        if nal.is_empty() {
            return;
        }
        let info = classify(self.codec, nal);
        let target = if info.param_set {
            &mut self.cur.config
        } else {
            &mut self.cur.data
        };
        target.extend_from_slice(&START);
        target.extend_from_slice(nal);
        self.cur.key |= info.key;
        self.cur_has_vcl |= info.vcl;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream(nals: &[&[u8]]) -> Vec<u8> {
        let mut v = Vec::new();
        for n in nals {
            v.extend_from_slice(&[0, 0, 0, 1]);
            v.extend_from_slice(n);
        }
        v
    }

    #[test]
    fn avc_two_frames_with_params() {
        let sps: &[u8] = &[0x67, 1, 2, 3];
        let pps: &[u8] = &[0x68, 4];
        let idr: &[u8] = &[0x65, 0x88, 9, 9];
        let p1: &[u8] = &[0x41, 0x9a, 7];
        let p2: &[u8] = &[0x41, 0x9b, 7];
        let s = stream(&[sps, pps, idr, p1, p2]);
        let mut sp = AnnexBSplitter::new(Codec::Avc);
        let mut out = Vec::new();
        // in kleinen Stücken einspeisen
        for c in s.chunks(3) {
            sp.push(c, &mut out);
        }
        assert_eq!(out.len(), 2, "zwei Bilder fertig, drittes wartet");
        assert!(out[0].key);
        assert_eq!(out[0].config, stream(&[sps, pps]));
        assert_eq!(out[0].data, stream(&[idr]));
        assert!(!out[1].key);
        sp.flush_pending(&mut out);
        assert_eq!(out.len(), 3);
        assert_eq!(out[2].data, stream(&[p2]));
    }

    #[test]
    fn avc_multi_slice_frame_stays_together() {
        let s1: &[u8] = &[0x65, 0x88, 1]; // first_mb = 0
        let s2: &[u8] = &[0x65, 0x40, 2]; // first_mb != 0
        let p: &[u8] = &[0x41, 0x9a, 3];
        let mut sp = AnnexBSplitter::new(Codec::Avc);
        let mut out = Vec::new();
        sp.push(&stream(&[s1, s2, p]), &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].data, stream(&[s1, s2]));
    }

    #[test]
    fn hevc_frames() {
        let vps: &[u8] = &[0x40, 0x01, 1];
        let sps: &[u8] = &[0x42, 0x01, 2];
        let pps: &[u8] = &[0x44, 0x01, 3];
        let idr: &[u8] = &[0x26, 0x01, 0xaf, 5]; // IDR_W_RADL (19)
        let trail: &[u8] = &[0x02, 0x01, 0xd0, 6]; // TRAIL_R (1)
        let aud: &[u8] = &[0x46, 0x01, 0x50];
        let mut sp = AnnexBSplitter::new(Codec::Hevc);
        let mut out = Vec::new();
        sp.push(
            &stream(&[aud, vps, sps, pps, idr, aud, trail, aud]),
            &mut out,
        );
        assert_eq!(out.len(), 2);
        assert!(out[0].key);
        assert_eq!(out[0].config, stream(&[vps, sps, pps]));
        assert!(!out[1].key);
        assert_eq!(out[1].data, stream(&[aud, trail]));
    }

    #[test]
    fn idle_flush_emits_immediately() {
        let idr: &[u8] = &[0x65, 0x88, 9, 9];
        let p1: &[u8] = &[0x41, 0x9a, 7];
        let mut sp = AnnexBSplitter::new(Codec::Avc);
        let mut out = Vec::new();
        sp.push(&stream(&[idr]), &mut out);
        assert!(out.is_empty());
        sp.flush_pending(&mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].data, stream(&[idr]));
        // Fortsetzungsmüll ohne Startcode wird verworfen, danach normal weiter
        sp.push(&[0x12, 0x34], &mut out);
        sp.push(&stream(&[p1]), &mut out);
        sp.flush_pending(&mut out);
        assert_eq!(out.len(), 2);
        assert_eq!(out[1].data, stream(&[p1]));
    }

    #[test]
    fn three_byte_start_codes() {
        let mut s = vec![
            0, 0, 1, 0x65, 0x88, 1, 0, 0, 1, 0x41, 0x9a, 2, 0, 0, 1, 0x41, 0x9a, 3,
        ];
        s.push(0); // trailing zero
        let mut sp = AnnexBSplitter::new(Codec::Avc);
        let mut out = Vec::new();
        sp.push(&s, &mut out);
        sp.flush_pending(&mut out);
        assert_eq!(out.len(), 3);
        assert_eq!(out[2].data, vec![0, 0, 0, 1, 0x41, 0x9a, 3]);
    }
}
