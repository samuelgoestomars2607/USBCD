//! Touch und S Pen vom Tablet als echte Windows-Eingabe.
//!
//! Windows bietet dafür synthetische Zeigergeräte (ab Windows 10 1809): Für Apps sieht
//! das genauso aus wie ein Stift- bzw. Touch-Bildschirm – mit Druckstufen, Neigung,
//! Schweben (Hover) und Seitentaste. Programme mit „Windows Ink“ bzw. Zeiger-
//! Eingabe (Affinity, Krita, Photoshop, OneNote, Browser, …) bekommen den Druck.
//!
//! Nachricht INPUT (Tablet → PC), Koordinaten 0..65535 über die ganze Tablet-Fläche:
//!   Stift:  [1][flags][x u16][y u16][druck u16 (0..1024)][neigung_x i8][neigung_y i8]
//!           flags: 1 = in Reichweite, 2 = berührt, 4 = Seitentaste, 8 = Radierer
//!   Touch:  [2][anzahl] + je Finger [id][flags][x u16][y u16]
//!           flags: 1 = berührt (0 = gerade losgelassen), 2 = abgebrochen

use crate::display::{Placement, VirtualDisplay};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::POINT;
use windows_sys::Win32::UI::Controls::{
    CreateSyntheticPointerDevice, DestroySyntheticPointerDevice, HSYNTHETICPOINTERDEVICE,
    POINTER_FEEDBACK_DEFAULT, POINTER_TYPE_INFO,
};
use windows_sys::Win32::UI::Input::Pointer::{
    InjectSyntheticPointerInput, POINTER_FLAG_CANCELED, POINTER_FLAG_DOWN, POINTER_FLAG_INCONTACT,
    POINTER_FLAG_INRANGE, POINTER_FLAG_UP, POINTER_FLAG_UPDATE,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    PEN_FLAG_BARREL, PEN_FLAG_ERASER, PEN_FLAG_INVERTED, PEN_MASK_PRESSURE, PEN_MASK_TILT_X,
    PEN_MASK_TILT_Y, PT_PEN, PT_TOUCH, TOUCH_FLAG_NONE, TOUCH_MASK_NONE,
};

pub const MAX_TOUCH: usize = 10;
/// Nach dem Stift so lange keine neuen Finger annehmen (Handballen beim Zeichnen).
const PALM_GUARD: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Pen {
    pub in_range: bool,
    pub contact: bool,
    pub barrel: bool,
    pub eraser: bool,
    pub x: u16,
    pub y: u16,
    pub pressure: u16,
    pub tilt_x: i8,
    pub tilt_y: i8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Touch {
    pub id: u8,
    pub down: bool,
    pub canceled: bool,
    pub x: u16,
    pub y: u16,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    Pen(Pen),
    Touch(Vec<Touch>),
}

/// Druckkurve des S Pen (0–1024): „weich“ = wenig Kraft für kräftige Striche, „hart“ =
/// feinere Abstufung bei leichtem Druck. Linear (Standard) bleibt unverändert. Anfang und
/// Ende bleiben gleich, ein berührender Stift behält immer etwas Druck.
pub fn pressure_curve(p: u16, curve: PenPressure) -> u16 {
    let exp = match curve {
        PenPressure::Soft => 0.6,
        PenPressure::Hard => 1.6,
        PenPressure::Linear => return p,
    };
    if p == 0 {
        return 0;
    }
    let x = (p.min(1024) as f32 / 1024.0).powf(exp);
    ((x * 1024.0).round() as u16).clamp(1, 1024)
}

/// Was die Seitentaste des S Pen macht (Menü „Seitentaste des Stifts“).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ButtonMode {
    /// Wie unter Windows üblich: Taste halten und tippen = Rechtsklick. Zusätzlich gibt ein
    /// Klick, während der Stift nur schwebt, einen Rechtsklick an dieser Stelle.
    #[default]
    RightClick,
    /// Solange die Taste gedrückt ist, ist der Stift ein Radierer (wie das Radierer-Ende
    /// am Surface Pen).
    Eraser,
}

crate::text_choice!(ButtonMode {
    RightClick = "right_click",
    Eraser = "eraser",
});

/// Druckkurve des S Pen (Menü „Druckempfindlichkeit“, `pen_pressure` in usbcd.toml).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PenPressure {
    /// weich – wenig Kraft nötig
    Soft,
    #[default]
    Linear,
    /// hart – feiner bei leichtem Druck
    Hard,
}
crate::text_choice!(PenPressure {
    Soft = "soft",
    Linear = "linear",
    Hard = "hard",
});

/// Was ein Stift-Ereignis nach der Seitentaste bewirkt.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Shaped {
    /// das, was Windows als Stift sieht
    pub pen: Pen,
    /// vorher einen Rechtsklick (Tippen mit Seitentaste) an der alten Stelle
    pub right_tap: bool,
    /// Radierer an/aus beim Schweben: Windows erwartet dafür, dass der Stift kurz die
    /// Reichweite verlässt (wie beim Umdrehen eines echten Stifts)
    pub reenter: bool,
}

/// Merkt sich den Zustand der Seitentaste über die Ereignisse hinweg.
#[derive(Default)]
pub struct PenButton {
    mode: ButtonMode,
    pressed: bool,
    /// während des Drückens berührt: kein Klick in der Luft
    touched: bool,
}

impl PenButton {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_mode(&mut self, m: ButtonMode) {
        if m != self.mode {
            self.mode = m;
            self.pressed = false;
            self.touched = false;
        }
    }

    /// `prev`: was Windows zuletzt gesehen hat, `cur`: was das Tablet gerade meldet.
    pub fn shape(&mut self, prev: &Pen, cur: Pen) -> Shaped {
        let pressed = cur.barrel && cur.in_range;
        let press = pressed && !self.pressed;
        let release = !pressed && self.pressed;
        if press {
            self.touched = cur.contact;
        } else if pressed && cur.contact {
            self.touched = true;
        }
        self.pressed = pressed;
        let mut out = Shaped {
            pen: cur,
            ..Default::default()
        };
        match self.mode {
            ButtonMode::RightClick => {
                out.right_tap = release && !self.touched && cur.in_range && !cur.contact;
            }
            ButtonMode::Eraser => {
                out.pen.barrel = false;
                let want = cur.eraser || cur.barrel;
                if prev.contact {
                    // nie mitten im Strich (oder beim Abheben) umschalten
                    out.pen.eraser = prev.eraser;
                } else {
                    out.pen.eraser = want;
                    out.reenter = prev.in_range && !prev.contact && prev.eraser != want;
                }
            }
        }
        out
    }
}

fn u16_at(b: &[u8], i: usize) -> u16 {
    u16::from_be_bytes([b[i], b[i + 1]])
}

/// Liest eine INPUT-Nachricht; unbekanntes oder Kaputtes wird verworfen.
pub fn parse(b: &[u8]) -> Option<Event> {
    match *b.first()? {
        1 if b.len() == 10 => {
            let f = b[1];
            Some(Event::Pen(Pen {
                in_range: f & 1 != 0 || f & 2 != 0,
                contact: f & 2 != 0,
                barrel: f & 4 != 0,
                eraser: f & 8 != 0,
                x: u16_at(b, 2),
                y: u16_at(b, 4),
                pressure: u16_at(b, 6).min(1024),
                tilt_x: (b[8] as i8).clamp(-90, 90),
                tilt_y: (b[9] as i8).clamp(-90, 90),
            }))
        }
        2 if b.len() >= 2 => {
            let n = b[1] as usize;
            if n > MAX_TOUCH || b.len() != 2 + n * 6 {
                return None;
            }
            let mut v = Vec::with_capacity(n);
            for i in 0..n {
                let o = 2 + i * 6;
                if b[o] as usize >= MAX_TOUCH {
                    return None;
                }
                v.push(Touch {
                    id: b[o],
                    down: b[o + 1] & 1 != 0,
                    canceled: b[o + 1] & 2 != 0,
                    x: u16_at(b, o + 2),
                    y: u16_at(b, o + 4),
                });
            }
            Some(Event::Touch(v))
        }
        _ => None,
    }
}

/// Zeiger-Flags für den Übergang des Stifts von `prev` nach `cur` (kann zwei Schritte
/// ergeben, z. B. Abheben und gleichzeitig die Reichweite verlassen).
pub fn pen_steps(prev: &Pen, cur: &Pen) -> Vec<u32> {
    let r = POINTER_FLAG_INRANGE;
    let c = POINTER_FLAG_INCONTACT;
    let mut v = Vec::new();
    match (prev.contact, cur.contact) {
        (false, true) => {
            if !prev.in_range {
                v.push(r | POINTER_FLAG_UPDATE); // erst „schwebt“, dann „berührt“
            }
            v.push(r | c | POINTER_FLAG_DOWN);
        }
        (true, true) => v.push(r | c | POINTER_FLAG_UPDATE),
        (true, false) => {
            v.push(r | POINTER_FLAG_UP);
            if !cur.in_range {
                v.push(POINTER_FLAG_UPDATE);
            }
        }
        (false, false) => {
            if cur.in_range {
                v.push(r | POINTER_FLAG_UPDATE);
            } else if prev.in_range {
                v.push(POINTER_FLAG_UPDATE); // Reichweite verlassen
            }
        }
    }
    v
}

/// Bildschirm-Lage (Hauptbildschirm = 0,0) → Lage relativ zum Desktop-Ursprung.
pub fn to_virtual(p: Placement, ox: i32, oy: i32) -> Placement {
    Placement {
        x: p.x - ox,
        y: p.y - oy,
        ..p
    }
}

/// Tablet-Koordinate (0..65535) → Bildschirm-Pixel des Tablet-Monitors.
pub fn map(x: u16, y: u16, a: &Placement) -> POINT {
    let span = |v: u16, len: u32| (v as i64 * (len.max(1) as i64 - 1) / 65535) as i32;
    POINT {
        x: a.x + span(x, a.width),
        y: a.y + span(y, a.height),
    }
}

pub struct Injector {
    vd: VirtualDisplay,
    pen_dev: HSYNTHETICPOINTERDEVICE,
    touch_dev: HSYNTHETICPOINTERDEVICE,
    area: Option<Placement>,
    /// Zeitpunkt der letzten Abfrage (None = noch nie)
    area_at: Option<Instant>,
    pen: Pen,
    button: PenButton,
    pen_seen: Option<Instant>,
    touches: Vec<Touch>,
    errors: u32,
}

impl Injector {
    pub fn new(vd: VirtualDisplay) -> Self {
        let (pen_dev, touch_dev) = unsafe {
            (
                CreateSyntheticPointerDevice(PT_PEN, 1, POINTER_FEEDBACK_DEFAULT),
                CreateSyntheticPointerDevice(PT_TOUCH, MAX_TOUCH as u32, POINTER_FEEDBACK_DEFAULT),
            )
        };
        if pen_dev.is_null() || touch_dev.is_null() {
            crate::log!(
                "Touch/Stift: Windows-Eingabegerät nicht anlegbar ({})",
                std::io::Error::last_os_error()
            );
        } else {
            crate::log!("Touch/Stift: Eingabe vom Tablet aktiv");
        }
        Injector {
            vd,
            pen_dev,
            touch_dev,
            area: None,
            area_at: None,
            pen: Pen::default(),
            button: PenButton::new(),
            pen_seen: None,
            touches: Vec::new(),
            errors: 0,
        }
    }

    /// Lage des Tablet-Monitors in den Koordinaten der Eingabe-Schnittstelle; wird
    /// regelmäßig neu gelesen, falls du ihn verschiebst.
    ///
    /// Windows erwartet die Position hier relativ zur linken oberen Ecke des GESAMTEN
    /// Desktops (nicht des Hauptbildschirms). Liegt das Tablet links vom Hauptmonitor
    /// (x negativ), landeten Eingaben sonst außerhalb und wurden still verworfen.
    fn area(&mut self) -> Option<Placement> {
        if self
            .area_at
            .is_none_or(|t| t.elapsed() > Duration::from_millis(750))
        {
            let (ox, oy) = crate::display::virtual_origin();
            let next = crate::display::current_placement(&self.vd).map(|p| to_virtual(p, ox, oy));
            if next.is_none() && self.area.is_some() {
                crate::log!("Touch/Stift: Lage des Tablet-Monitors unbekannt");
            }
            if next != self.area {
                if let Some(a) = next {
                    crate::log!(
                        "Touch/Stift: Tablet-Fläche {}x{} ab {},{} (Desktop-Ursprung {ox},{oy})",
                        a.width,
                        a.height,
                        a.x,
                        a.y
                    );
                }
            }
            self.area = next;
            self.area_at = Some(Instant::now());
        }
        self.area
    }

    pub fn handle(&mut self, ev: Event) {
        match ev {
            Event::Pen(p) => self.pen(p),
            Event::Touch(t) => self.touch(t),
        }
    }

    fn inject(&mut self, dev: HSYNTHETICPOINTERDEVICE, info: &[POINTER_TYPE_INFO]) {
        if dev.is_null() || info.is_empty() {
            return;
        }
        let ok = unsafe { InjectSyntheticPointerInput(dev, info.as_ptr(), info.len() as u32) };
        if ok == 0 {
            self.errors += 1;
            if self.errors <= 3 {
                crate::log!(
                    "Touch/Stift an Windows übergeben: {}",
                    std::io::Error::last_os_error()
                );
            }
        }
    }

    pub fn set_button_mode(&mut self, m: ButtonMode) {
        self.button.set_mode(m);
    }

    fn pen_info(flags: u32, cur: &Pen, pt: POINT) -> POINTER_TYPE_INFO {
        let mut pen_flags = 0;
        if cur.barrel {
            pen_flags |= PEN_FLAG_BARREL;
        }
        if cur.eraser {
            pen_flags |= PEN_FLAG_INVERTED;
            if flags & POINTER_FLAG_INCONTACT != 0 {
                pen_flags |= PEN_FLAG_ERASER;
            }
        }
        let mut pi: POINTER_TYPE_INFO = unsafe { std::mem::zeroed() };
        pi.r#type = PT_PEN;
        let p = unsafe { &mut pi.Anonymous.penInfo };
        p.pointerInfo.pointerType = PT_PEN;
        p.pointerInfo.pointerId = 0;
        p.pointerInfo.pointerFlags = flags;
        p.pointerInfo.ptPixelLocation = pt;
        p.pointerInfo.historyCount = 1;
        p.penFlags = pen_flags;
        p.penMask = PEN_MASK_PRESSURE | PEN_MASK_TILT_X | PEN_MASK_TILT_Y;
        p.pressure = if flags & POINTER_FLAG_INCONTACT != 0 {
            (cur.pressure as u32).max(1)
        } else {
            0
        };
        p.tiltX = cur.tilt_x as i32;
        p.tiltY = cur.tilt_y as i32;
        pi
    }

    fn pen(&mut self, raw: Pen) {
        let Some(a) = self.area() else { return };
        // Stift kommt: liegende Finger (meist der Handballen) abbrechen
        if raw.in_range && !self.touches.is_empty() {
            self.cancel_touches();
        }
        if raw.in_range || raw.contact {
            self.pen_seen = Some(Instant::now());
        }
        let mut prev = self.pen;
        let shaped = self.button.shape(&prev, raw);
        let cur = shaped.pen;
        let mut infos = Vec::new();
        if shaped.right_tap {
            // Klick in der Luft: an der Stelle mit gedrückter Seitentaste kurz tippen –
            // genau das, was Windows sonst als Rechtsklick vom Stift kennt
            let at = map(prev.x, prev.y, &a);
            let tap = Pen {
                barrel: true,
                pressure: 512,
                ..prev
            };
            let r = POINTER_FLAG_INRANGE;
            infos.push(Self::pen_info(
                r | POINTER_FLAG_INCONTACT | POINTER_FLAG_DOWN,
                &tap,
                at,
            ));
            infos.push(Self::pen_info(r | POINTER_FLAG_UP, &tap, at));
        }
        if shaped.reenter {
            let at = map(prev.x, prev.y, &a);
            infos.push(Self::pen_info(POINTER_FLAG_UPDATE, &prev, at));
            prev.in_range = false;
        }
        let pt = map(cur.x, cur.y, &a);
        for flags in pen_steps(&prev, &cur) {
            infos.push(Self::pen_info(flags, &cur, pt));
        }
        // jeder Schritt einzeln (ein Stift = ein Zeiger pro Aufruf)
        let dev = self.pen_dev;
        for i in infos {
            self.inject(dev, std::slice::from_ref(&i));
        }
        self.pen = cur;
    }

    fn palm_guard(&self) -> bool {
        self.pen.in_range || self.pen_seen.is_some_and(|t| t.elapsed() < PALM_GUARD)
    }

    fn touch_info(t: &Touch, flags: u32, a: &Placement) -> POINTER_TYPE_INFO {
        let mut pi: POINTER_TYPE_INFO = unsafe { std::mem::zeroed() };
        pi.r#type = PT_TOUCH;
        let ti = unsafe { &mut pi.Anonymous.touchInfo };
        ti.pointerInfo.pointerType = PT_TOUCH;
        ti.pointerInfo.pointerId = t.id as u32;
        ti.pointerInfo.pointerFlags = flags;
        ti.pointerInfo.ptPixelLocation = map(t.x, t.y, a);
        ti.pointerInfo.historyCount = 1;
        ti.touchFlags = TOUCH_FLAG_NONE;
        ti.touchMask = TOUCH_MASK_NONE;
        pi
    }

    fn touch(&mut self, set: Vec<Touch>) {
        let Some(a) = self.area() else { return };
        let guard = self.palm_guard();
        let mut infos = Vec::new();
        let mut next = Vec::new();
        for t in &set {
            let was = self.touches.iter().find(|o| o.id == t.id).copied();
            match (was.is_some(), t.down && !t.canceled) {
                (false, true) if !guard => {
                    infos.push(Self::touch_info(
                        t,
                        POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT | POINTER_FLAG_DOWN,
                        &a,
                    ));
                    next.push(*t);
                }
                (true, true) => {
                    infos.push(Self::touch_info(
                        t,
                        POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT | POINTER_FLAG_UPDATE,
                        &a,
                    ));
                    next.push(*t);
                }
                (true, false) => {
                    let mut f = POINTER_FLAG_UP;
                    if t.canceled {
                        f |= POINTER_FLAG_CANCELED;
                    }
                    infos.push(Self::touch_info(t, f, &a));
                }
                _ => {} // neuer Finger während des Zeichnens oder schon wieder weg
            }
        }
        // Finger, die das Tablet nicht mehr meldet, sauber beenden
        for o in &self.touches {
            if !set.iter().any(|t| t.id == o.id) {
                infos.push(Self::touch_info(
                    o,
                    POINTER_FLAG_UP | POINTER_FLAG_CANCELED,
                    &a,
                ));
            }
        }
        let dev = self.touch_dev;
        self.inject(dev, &infos);
        self.touches = next;
    }

    fn cancel_touches(&mut self) {
        let Some(a) = self.area else {
            self.touches.clear();
            return;
        };
        let infos: Vec<_> = self
            .touches
            .iter()
            .map(|t| Self::touch_info(t, POINTER_FLAG_UP | POINTER_FLAG_CANCELED, &a))
            .collect();
        let dev = self.touch_dev;
        self.inject(dev, &infos);
        self.touches.clear();
    }
}

impl Drop for Injector {
    fn drop(&mut self) {
        // Verbindung weg: nichts „gedrückt“ hängen lassen
        if self.pen.in_range || self.pen.contact {
            let gone = Pen {
                in_range: false,
                contact: false,
                ..self.pen
            };
            self.pen(gone);
        }
        self.cancel_touches();
        unsafe {
            if !self.pen_dev.is_null() {
                DestroySyntheticPointerDevice(self.pen_dev);
            }
            if !self.touch_dev.is_null() {
                DestroySyntheticPointerDevice(self.touch_dev);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hover(barrel: bool) -> Pen {
        Pen {
            in_range: true,
            barrel,
            x: 100,
            y: 200,
            ..Default::default()
        }
    }

    fn touching(barrel: bool) -> Pen {
        Pen {
            contact: true,
            pressure: 300,
            ..hover(barrel)
        }
    }

    #[test]
    fn side_button_right_click_in_the_air() {
        let mut b = PenButton::new();
        let mut seen = Pen::default();
        let mut feed = |b: &mut PenButton, p: Pen| {
            let s = b.shape(&seen, p);
            seen = s.pen;
            s
        };
        assert!(!feed(&mut b, hover(false)).right_tap);
        let s = feed(&mut b, hover(true));
        assert!(!s.right_tap && s.pen.barrel, "Taste geht weiter an Windows");
        assert!(feed(&mut b, hover(false)).right_tap, "Klick in der Luft");
        // halten und tippen: Windows macht den Rechtsklick selbst, kein zweiter
        feed(&mut b, hover(true));
        feed(&mut b, touching(true));
        feed(&mut b, hover(true));
        assert!(!feed(&mut b, hover(false)).right_tap);
        // Stift mit gedrückter Taste aus der Reichweite gezogen: nichts
        feed(&mut b, hover(true));
        assert!(!feed(&mut b, Pen::default()).right_tap);
    }

    #[test]
    fn side_button_as_eraser() {
        let mut b = PenButton::new();
        b.set_mode(ButtonMode::Eraser);
        let mut seen = Pen::default();
        let mut feed = |b: &mut PenButton, p: Pen| {
            let s = b.shape(&seen, p);
            seen = s.pen;
            s
        };
        let s = feed(&mut b, hover(false));
        assert!(!s.pen.eraser && !s.reenter);
        let s = feed(&mut b, hover(true));
        assert!(
            s.pen.eraser && !s.pen.barrel && s.reenter,
            "beim Schweben umschalten"
        );
        let s = feed(&mut b, touching(true));
        assert!(s.pen.eraser && !s.reenter);
        // Taste mitten im Strich losgelassen: Strich bleibt Radierer, auch beim Abheben
        let s = feed(&mut b, touching(false));
        assert!(s.pen.eraser && !s.reenter);
        let s = feed(&mut b, hover(false));
        assert!(s.pen.eraser && !s.reenter);
        let s = feed(&mut b, hover(false));
        assert!(!s.pen.eraser && s.reenter, "danach beim Schweben zurück");
        // Taste mitten im Strich gedrückt: Strich bleibt Stift
        feed(&mut b, touching(false));
        let s = feed(&mut b, touching(true));
        assert!(!s.pen.eraser && !s.pen.barrel);
    }

    #[test]
    fn parses_pen_and_touch() {
        let pen = [1, 3, 0x80, 0, 0x40, 0, 0x02, 0x00, 10, 0xF6];
        assert_eq!(
            parse(&pen),
            Some(Event::Pen(Pen {
                in_range: true,
                contact: true,
                barrel: false,
                eraser: false,
                x: 0x8000,
                y: 0x4000,
                pressure: 512,
                tilt_x: 10,
                tilt_y: -10,
            }))
        );
        let touch = [2, 1, 3, 1, 0, 10, 0, 20];
        assert_eq!(
            parse(&touch),
            Some(Event::Touch(vec![Touch {
                id: 3,
                down: true,
                canceled: false,
                x: 10,
                y: 20
            }]))
        );
        // genau so erzeugt von der Android-App (InputForwarder.penBytes, auf dem JVM geprüft)
        let from_app = [1, 3, 127, 255, 63, 255, 2, 0, 28, 0];
        match parse(&from_app) {
            Some(Event::Pen(p)) => {
                assert!(p.contact && p.in_range);
                assert_eq!(
                    (p.x, p.y, p.pressure, p.tilt_x, p.tilt_y),
                    (32767, 16383, 512, 28, 0)
                );
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(parse(&[2, 11]), None);
        assert_eq!(parse(&[2, 1, 12, 1, 0, 0, 0, 0]), None); // id zu groß
        assert_eq!(parse(&[1, 0, 0]), None);
        assert_eq!(parse(&[9]), None);
        assert_eq!(parse(&[]), None);
    }

    #[test]
    fn pen_sequence() {
        let away = Pen::default();
        let hover = Pen {
            in_range: true,
            ..away
        };
        let down = Pen {
            contact: true,
            ..hover
        };
        let r = POINTER_FLAG_INRANGE;
        let c = POINTER_FLAG_INCONTACT;
        assert_eq!(pen_steps(&away, &hover), vec![r | POINTER_FLAG_UPDATE]);
        assert_eq!(pen_steps(&hover, &down), vec![r | c | POINTER_FLAG_DOWN]);
        assert_eq!(pen_steps(&down, &down), vec![r | c | POINTER_FLAG_UPDATE]);
        assert_eq!(pen_steps(&down, &hover), vec![r | POINTER_FLAG_UP]);
        assert_eq!(pen_steps(&hover, &away), vec![POINTER_FLAG_UPDATE]);
        assert_eq!(pen_steps(&away, &away), Vec::<u32>::new());
        // direkt aufgesetzt bzw. direkt weggezogen
        assert_eq!(
            pen_steps(&away, &down),
            vec![r | POINTER_FLAG_UPDATE, r | c | POINTER_FLAG_DOWN]
        );
        assert_eq!(
            pen_steps(&down, &away),
            vec![r | POINTER_FLAG_UP, POINTER_FLAG_UPDATE]
        );
    }

    #[test]
    fn left_monitor_becomes_positive() {
        // Tablet links vom Hauptmonitor: Desktop beginnt bei x = -2000
        let tablet = Placement {
            x: -2000,
            y: 240,
            width: 2000,
            height: 1200,
        };
        let v = to_virtual(tablet, -2000, 0);
        assert_eq!((v.x, v.y), (0, 240));
        let p = map(65535, 0, &v);
        assert_eq!((p.x, p.y), (1999, 240));
    }

    #[test]
    fn maps_to_monitor() {
        let a = Placement {
            x: -2000,
            y: 120,
            width: 2000,
            height: 1200,
        };
        let p = map(0, 0, &a);
        assert_eq!((p.x, p.y), (-2000, 120));
        let p = map(65535, 65535, &a);
        assert_eq!((p.x, p.y), (-1, 1319));
    }

    #[test]
    fn pressure_curves() {
        for c in [PenPressure::Linear, PenPressure::Soft, PenPressure::Hard] {
            assert_eq!(pressure_curve(0, c), 0);
            assert_eq!(pressure_curve(1024, c), 1024);
            assert!(pressure_curve(1, c) >= 1);
        }
        assert_eq!(pressure_curve(512, PenPressure::Linear), 512);
        assert!(pressure_curve(256, PenPressure::Soft) > 256);
        assert!(pressure_curve(256, PenPressure::Hard) < 256);
        // stetig steigend
        let mut last = 0;
        for p in 0..=1024 {
            let v = pressure_curve(p, PenPressure::Hard);
            assert!(v >= last);
            last = v;
        }
    }
}
