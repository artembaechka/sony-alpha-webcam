# sony-remote

Open-source live view, remote control and virtual webcam for Sony Alpha
cameras over USB, written in Rust. A replacement for Sony's *Imaging Edge
Webcam* that needs **no driver installation**.

Tested with an **ILCE-6300 (A6300)**, firmware 2.0, on Windows 11.

## Features

- Live view at ~30 fps (1024×576 JPEG from the camera).
- Exposure: aperture, shutter speed, ISO, exposure compensation, white balance.
- Focus: hold-to-autofocus (shutter half press), focus status.
- Virtual webcam “Sony Camera (sony-remote)” for Zoom, Teams, OBS, Discord
  and browsers (1280×720 NV12 @ 30 fps).
- Reconnects automatically after the cable is unplugged or the camera is
  switched off.

## How it works

| Layer | What it does |
|---|---|
| `crates/sony-ptp` | Sony PTP "PC Remote" protocol: SDIO handshake (`0x9201`/`0x9202`), property dump (`0x9209`), property control (`0x9205`/`0x9207`), live view (`GetObject 0xFFFFC002`). OS independent, unit tested. |
| `crates/ptp-wpd` | Transport over Windows Portable Devices. In *PC Remote* mode Windows binds the camera to its inbox MTP driver; raw PTP (including Sony vendor operations) is sent with `WPD_COMMAND_MTP_EXT_*`. No WinUSB/libusb driver swap. |
| `crates/vcam-source` | Media Foundation media source DLL, registered with `MFCreateVirtualCamera` (Windows 11). Loaded by the Camera Frame Server service. |
| `crates/vcam-shared` | Frame exchange between the app and the DLL (`Global\` shared memory, seqlock triple buffer) and RGBA → NV12 conversion. |
| `app` | `sony-remote.exe`: egui GUI, headless mode and diagnostic CLI. |

Notes on the A6300 (see `fixtures/a6300` for raw dumps):

- Aperture, shutter, ISO and exposure compensation can only be changed in
  one-notch steps, and each step needs ~0.5 s before the camera accepts the
  next one. Jumping far (e.g. ISO 100 → 6400) therefore takes a few seconds.
- Exposure is only adjustable with the mode dial on P/A/S/M.
- The A6300 does not offer remote manual focus; use the focus ring.

## Camera setup

1. MENU → Setup (toolbox) → **USB Connection** → **PC Remote**.
2. Connect the camera with a USB cable.
3. Close Sony Imaging Edge Webcam if it is running (it holds the camera).

## Build

Requires Rust (MSVC toolchain, see `rust-toolchain.toml`) and Windows 11.

```
cargo build --release
```

Produces `target\release\sony-remote.exe` and `target\release\sony_remote_vcam.dll`
(keep them in the same folder).

## Usage

```
sony-remote                 # GUI
sony-remote serve           # no window: keep the virtual webcam fed
sony-remote vcam install    # install the virtual webcam (UAC prompt, once)
sony-remote vcam uninstall
sony-remote vcam status
```

`vcam install` copies the DLL to `C:\Program Files\sony-remote\` (the Frame
Server service cannot read user folders), registers its COM class in HKLM and
creates a system-wide virtual camera. The GUI offers the same through the
*Install webcam* button.

While no `sony-remote` is running, the virtual camera shows a dark
placeholder image.

### Diagnostics

```
sony-remote list                          # portable devices, Sony ones marked with *
sony-remote probe --dump fixtures\mycam   # device info + every property, raw dumps
sony-remote liveview --frames 100 --out frames
sony-remote set D21E up                   # step ISO; codes are hex
sony-remote set 5005 0x4                  # white balance = daylight
sony-remote raw 9209/r 1009:FFFFC002/r    # raw PTP transactions (hex)
sony-remote --log debug ...               # verbose logging
```

The DLL logs through `OutputDebugString` (view with Sysinternals DebugView,
"Capture Global Win32").

## License

MIT OR Apache-2.0
