<p align="center">
  <img src="installer/USBCD-logo.png" width="128" height="128" alt="USBCD logo">
</p>

# USBCD – Android tablet as a second monitor with speakers (USB-C, Windows 11)

*Deutsche Version: [README.de.md](README.de.md)*

Connect the tablet via USB-C → Windows gets a **new monitor** with the tablet's native resolution and highest
refresh rate. There is also an output **"USBCD Tablet Speakers"** that you can choose in Windows.
When you unplug the tablet, the monitor disappears again.

Windows display settings also offer smaller resolutions in the tablet's aspect ratio (Tab S6 Lite: 1600 × 960,
1280 × 768, 1000 × 600). They are transmitted at that size and upscaled by the tablet – easier on weaker PCs.
For larger text, scaling stays the sharper choice.

Language: English or German – setup, Windows program and tablet app follow the respective system language.

## Requirements

* Windows 11 (64-bit)
* Android tablet with Android 8.0 or newer and a USB-C data cable – developed and tested with the Samsung Galaxy Tab S6 Lite
* Recommended: a GPU with a hardware encoder (NVIDIA, AMD or Intel). Without one it falls back to software, which is slower.

## How it works

An Android tablet cannot present itself as a monitor on its USB-C port: the port only works as a USB *device*
(MTP, ADB …) and has no video input. That's why there are two parts:

* **Windows (Rust, taskbar icon):** detects the tablet, starts the app, creates a real virtual monitor
  ([Virtual Display Driver](https://github.com/VirtualDrivers/Virtual-Display-Driver)) with the tablet's resolution
  and refresh rate, captures it with FFmpeg and encodes it on the GPU (NVENC / AMF / QSV). Sound played on
  "USBCD Tablet Speakers" (VB-CABLE) is captured and sent uncompressed.
* **Tablet app (Kotlin):** picks the highest display mode, reports resolution and decoder capabilities,
  decodes in hardware with low latency, plays the sound.
* **Connection:** ADB over the USB cable – no Wi-Fi needed.

The default sound output is **not** changed. For sound on the tablet, choose "USBCD Tablet Speakers" in Windows.

## Security

* **USBCD runs without administrator rights** (autostart via `HKCU\…\Run`). The few things that need rights –
  switching the virtual monitor on/off, setting its mode, naming the speaker – are done by the Windows service
  **"USBCD Helper"**. Over the local pipe `\\.\pipe\USBCD-Helper` it accepts only fixed, built-in commands with
  checked values, and only from the installed `USBCD.exe`. No paths, no scripts, no PowerShell.
* **Mutual sign-in PC ↔ tablet:** on launch the app receives a random secret via adb (kept in memory only, valid
  while USBCD runs). Every connection starts with an HMAC-SHA256 challenge-response in both directions. Another app
  occupying the USBCD port on the tablet therefore gets no picture, and nobody can feed the app a fake picture.
  The app accepts the secret only through the `.Connect` entry point, which only the adb shell may open.
* **Protected folders:** `C:\VirtualDisplayDriver` (driver settings) and `C:\ProgramData\USBCD` (service and
  setup logs) can only be changed by administrators.
* **Checked downloads:** the setup verifies VB-CABLE's signature (VB-Audio) before running it. The build verifies
  the signatures of adb (Google) and the monitor driver and pins SHA-256 checksums in `installer\deps.lock.json` –
  if a file changes unnoticed, the build stops.
* **Tablet app** signed with its own release key (`installer\keys\`, created on the first build – **back it up and
  keep it private**). A differently signed copy (e.g. from Android Studio) is replaced by USBCD when connecting.
* **Code signing (optional):** with `USBCD_SIGN_CERT_THUMBPRINT` (thumbprint of a code-signing certificate in the
  certificate store) the build signs `USBCD.exe`, the setup and the uninstaller.
* **USB debugging** gives the connected PC far-reaching access to the tablet. Choose "Always allow" only on your own
  PCs and remove other authorizations under *Developer options → Revoke USB debugging authorizations*.

## Setup

### Tablet (once)
1. Settings → About tablet → Software information → tap *Build number* 7 times.
2. Developer options → turn on **USB debugging**.

### Install
1. Download the latest **`USBCD-Setup-<version>.exe`** from [Releases](../../releases/latest) and run it.
   It explains what will happen; Windows asks up to twice about driver software
   (monitor, VB-CABLE) – click *Install* each time. Restart once after the first VB-CABLE installation.
2. Connect the tablet and confirm "Allow USB debugging?" with **"Always allow from this computer"**.
   **The app is transferred to the tablet automatically** and updated with new versions.

To build the setup yourself, see [Building from source](#building-from-source).

## Usage

USBCD runs as an **icon in the notification area** (possibly under ^). Click it to open the menu:

| Menu item | Effect |
|---|---|
| Status | e.g. "Connected: Galaxy Tab S6 Lite · 2000×1200 · 60 Hz" |
| Show on tablet | open the app again after it was closed on the tablet |
| Devices | all known devices; check mark = use as screen, click toggles. New devices appear as “New: … – use as screen” |
| Send sound to tablet | sound transmission on/off |
| Touch and pen on the tablet | fingers and S Pen operate Windows on the tablet monitor (default: on) |
| Info overlay on the tablet | frames/s, data rate, decode time and USB round trip at the top left (default: off) |
| Turn tablet off and on with the PC screen | when the PC screen turns off or the PC is locked, the tablet display turns off too – and back on (default: on) |
| Tablet selfie camera as webcam | adds the camera “USBCD Tablet Webcam” to Windows (default: off) – see below |
| “Do not disturb” on the tablet while it is a monitor | no notifications over the picture; afterwards as before. Needs a one-time permission on the tablet (default: off) |
| Tablet volume buttons control the tablet speakers | the tablet buttons turn “USBCD Tablet Speakers” in Windows up/down, shown on the tablet (default: on) |
| Switch tablet off (picture and sound) | removes the tablet monitor and turns the tablet display off without unplugging; the same item switches it back on |
| Picture quality | Data saver (10 Mbit/s) · Standard (automatic) · High (40 Mbit/s) |
| Tablet brightness | 100 % · 75 % · 50 % · 25 % – applies immediately |
| Sound settings / Display settings | opens the Windows settings |
| Open settings and log | folder with `usbcd.toml` and `usbcd.log` |
| Start automatically at sign-in | autostart on/off |
| Quit USBCD | |

**Behaviour:**
* **New device:** USBCD installs or starts nothing without asking. A "New device detected" notification offers
  *Use as screen* and *Never for this device*. If you ignore it, nothing happens. The answer is remembered for this
  device and can be changed in the menu under *Devices*. Devices that already have the USBCD app count as allowed.
  Emulators and wireless debugging are never touched.
* For allowed devices the app opens automatically when connected; Windows shows "Tablet connected".
* Notifications come from "USBCD" with its logo; status messages are silent, errors play a sound and offer "Open log".
* **Wallpaper:** Windows often loses the wallpaper when virtual monitors are added or removed (black desktop). USBCD
  remembers each screen's picture beforehand and sets it again afterwards; the tablet monitor gets the main screen's
  picture. Slideshows, Windows Spotlight and solid-colour backgrounds are left alone.
* If you close the app, it stays closed – continue via *Show on tablet* or open the app yourself.
* Short USB dropouts are bridged silently in ~2 s; the monitor stays.
* Without a connection the tablet shows a calm animated background with status – never the frozen last frame.
* Problems (USB debugging not allowed, app missing, transmission errors) are reported by Windows with the next step.
* The tablet's volume is not changed.

**Touch and S Pen:** the pen arrives in Windows as a real pen – with pressure, hover (the cursor follows the pen
without clicking), side button (= right-click) and, if the pen reports it, tilt. This works in every program that uses
Windows pen input ("Windows Ink"), e.g. Affinity, Krita, Photoshop, OneNote, browsers. Fingers tap, drag and scroll
like on a touch screen. If your palm rests on the screen while drawing, fingers are ignored until shortly after the
pen is lifted.
* Programs that only know the old WinTab driver get no pressure (that would need a driver).
* For security reasons Windows does not let touch/pen operate windows running with administrator rights (e.g. UAC).
* Samsung opens "Air Command" when the S Pen button is pressed while hovering. If you want the button as right-click,
  turn that off under *Settings → Advanced features → S Pen → Air Command*.

**Webcam:** with *Tablet selfie camera as webcam*, Windows 11 gets the camera “USBCD Tablet Webcam”. You pick it
yourself in each app (Teams, Discord, Zoom, browser, Windows Camera app) – USBCD never makes it the default camera and
only ever uses the front camera, never the rear camera.
* The tablet camera only runs while an app actually uses the webcam (Android then shows the green camera dot).
  Otherwise it is off.
* The first time, the tablet asks once “Allow camera access?”.
* Without a picture (tablet not connected, switched off, access denied) the webcam shows a hint image.
* Picture: 1280×720, up to 30 frames/s. The camera only exists while USBCD runs, and only for your Windows user.
* Microsoft Teams is picky about virtual cameras in general (OBS & co. too). If it does not work there, that is down
  to Teams; in Discord, Zoom and the browser the webcam should work normally.

**Tablet display with the PC:** the display really turns off (not just dark) and comes back by itself when the PC
wakes up or is unlocked. Picture and sound keep running meanwhile; the tablet monitor stays in Windows, so your
windows stay where they are. If the tablet has a screen lock, USBCD still appears directly above the lock screen.

## Troubleshooting

Everything is in `usbcd.log` (menu → *Open settings and log*). Start menu → **USBCD – Diagnosis** collects all
important details in `diagnose.txt`. The service and the setup log to `C:\ProgramData\USBCD`.

* **"Helper service missing":** run the setup again (*Services* → "USBCD Helper" must be *Running*).
* **"Security warning: another app … pretends to be USBCD":** check the tablet for unknown apps and restart it.
  The tablet then shows "Connection blocked".

* **No picture / it stops:** try menu → *Picture quality → Data saver*; otherwise `encoder = "software"` in `usbcd.toml`.
* **Monitor does not appear:** Device Manager → *Display adapters* must list "Virtual Display Driver"
  (intentionally disabled while no tablet is connected).
* **No sound:** is "USBCD Tablet Speakers" selected in Windows and *Send sound to tablet* on?
* **App cannot be installed:** follow the notification; uninstall an old app on the tablet if needed.

**Uninstall:** *Settings → Apps → USBCD*. Removes program, helper service, monitor device and its driver package,
autostart, Start menu entries, settings, logs (`%LOCALAPPDATA%\USBCD`, `C:\ProgramData\USBCD`) and
`C:\VirtualDisplayDriver`. It asks whether VB-CABLE and the app on the connected tablet should go too; if VB-CABLE
stays, it is reset to its original state. Parsec and other virtual monitors stay untouched. Kept on purpose: the adb
key `%USERPROFILE%\.android` (shared with Android Studio) and the source folder including `installer\keys\`.

## Limits

* USB debugging must stay on.
* Latency typically 1–3 frames – fine for office work, video and most games.
* One tablet at a time; pressure only in programs with Windows pen input (not WinTab).
* Windows menus and notifications use the system font (Space Grotesk only in the tablet app).

## Building from source

**Requires:**
* [Rust](https://rustup.rs) (stable) – including the "Visual Studio C++ Build Tools", which the Rust installer offers to install.
* [Android Studio](https://developer.android.com/studio) – brings the Android SDK and Java 17. Open the `android` folder once and let it sync.
* [Inno Setup](https://jrsoftware.org/isinfo.php) 6.6 or newer – if it is missing, the build script installs it via winget.

**Build:**
1. In the `installer` folder, double-click **`Setup bauen.cmd`**. It builds the Windows program **and the tablet app**,
   downloads FFmpeg, adb and the monitor driver the first time, and creates
   **`installer\Output\USBCD-Setup-<version>.exe`**. On errors the window stays open (log: `installer\build.log`).
   If you downloaded the project as a ZIP, Windows may ask once whether to run the script anyway – that is normal
   for downloaded scripts.
2. The first build creates your own signing key for the tablet app in `installer\keys\`. **Back it up and never
   upload it** (it is in `.gitignore`). Apps you build are therefore signed differently from the release builds;
   when you switch, USBCD replaces the app on the tablet.

The third-party programs (FFmpeg, adb, monitor driver) are not part of the repository. The build script downloads
fixed versions – FFmpeg and adb from the [build-deps-1](../../releases/tag/build-deps-1) release, the monitor driver from
its own project –, checks their signatures and compares them with the checksums in `installer\deps.lock.json`.
That way everyone builds with exactly the same files.
If Android Studio is missing or the `android` folder was never synced, the setup is still created, but **without the
tablet app** – the script shows a warning. You can also install the tablet app directly from Android Studio
(open the `android` folder → ▶ Run).

## Project structure

```
android/    Android Studio project (Kotlin, no third-party libraries)
            MainActivity (waiting screen, brightness), GradientBackground, Fonts,
            DisplayInfo, StreamServer, VideoDecoder, AudioPlayer, Proto, Auth
            res/values (English), res/values-de (German), adaptive icon with monochrome layer
host/       Rust: main (device loop, app installation), tray (menu), session, helper (service
            and access), auth (pairing), vdd, display, driver, video, nal, audio, autostart,
            i18n, config, proto, adb, util
installer/  Setup bauen.cmd · build-installer.ps1 · USBCD.iss (de/en) · setup-helper.ps1 · diagnose.ps1
            · deps.lock.json (checksums) · info pages · THIRD-PARTY.txt
```

## Contributing

Bug reports and ideas are welcome as [issues](../../issues). For bugs, `diagnose.txt` (Start menu →
**USBCD – Diagnosis**) or `usbcd.log` helps – look through it before uploading in case it contains anything personal.
Pull requests are welcome; all visible text exists in German and English.

## License

Copyright © 2026 Samuel E. Ramirez. USBCD is free software under the
[GNU General Public License v3.0 or later](LICENSE). Third-party components (FFmpeg, adb, Virtual Display Driver,
Space Grotesk, VB-CABLE) and their licenses: [installer/THIRD-PARTY.txt](installer/THIRD-PARTY.txt).

Samsung, Galaxy, Android, Windows and other names mentioned are trademarks of their respective owners. USBCD is an
independent project and is not affiliated with these companies.
