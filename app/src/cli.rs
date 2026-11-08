//! Command line diagnostics: list / probe / liveview.

use std::path::PathBuf;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use ptp_wpd::{list_devices, ComGuard, DeviceEntry, WpdTransport};
use sony_ptp::codes::{self, op};
use sony_ptp::format::format_value;
use sony_ptp::liveview::{extract_jpeg, jpeg_dimensions};
use sony_ptp::{DataPhase, SonyCamera, Transport};

/// Sony USB product ids of "Mass Storage" / "MTP" modes, where remote control
/// is not available.
fn mode_hint(d: &DeviceEntry) -> &'static str {
    match d.pid {
        Some(0x0ABA) => "  (Mass Storage mode, switch USB Connection to PC Remote)",
        Some(0x0AB9) => "  (MTP mode, switch USB Connection to PC Remote)",
        _ => "",
    }
}

pub fn find_device(id: Option<String>) -> Result<DeviceEntry> {
    let devices = list_devices()?;
    if let Some(id) = id {
        return devices
            .into_iter()
            .find(|d| d.id.eq_ignore_ascii_case(&id))
            .context("device not found");
    }
    let sony: Vec<_> = devices.into_iter().filter(DeviceEntry::is_sony).collect();
    match sony.into_iter().next() {
        Some(d) => Ok(d),
        None => bail!(
            "no Sony camera found. Turn the camera on, set Menu → Setup → USB Connection → PC Remote and connect it"
        ),
    }
}

pub fn list() -> Result<()> {
    let _com = ComGuard::new()?;
    let devices = list_devices()?;
    if devices.is_empty() {
        println!("no portable devices");
    }
    for d in devices {
        let mark = if d.is_sony() { "*" } else { " " };
        println!(
            "{mark} {} [{}] vid={} pid={}{}\n    {}",
            d.display_name(),
            d.manufacturer,
            d.vid.map_or("-".into(), |v| format!("{v:04X}")),
            d.pid.map_or("-".into(), |v| format!("{v:04X}")),
            mode_hint(&d),
            d.id
        );
    }
    Ok(())
}

fn open(device: Option<String>) -> Result<(DeviceEntry, SonyCamera<WpdTransport>)> {
    let entry = find_device(device)?;
    eprintln!("opening {} ({})", entry.display_name(), entry.id);
    let transport = WpdTransport::open(&entry.id)?;
    Ok((entry, SonyCamera::new(transport)))
}

pub fn probe(device: Option<String>, dump: Option<PathBuf>) -> Result<()> {
    let _com = ComGuard::new()?;
    let (_entry, mut cam) = open(device)?;
    if let Some(dir) = &dump {
        std::fs::create_dir_all(dir)?;
    }
    let save = |name: &str, data: &[u8]| -> Result<()> {
        if let Some(dir) = &dump {
            std::fs::write(dir.join(name), data)?;
        }
        Ok(())
    };

    match cam.transport_mut().supported_vendor_opcodes() {
        Ok(ops) => println!(
            "driver forwards vendor opcodes: {}",
            ops.iter().map(|o| format!("0x{o:04X}")).collect::<Vec<_>>().join(" ")
        ),
        Err(e) => println!("vendor opcode query failed: {e}"),
    }

    let raw = cam.call(op::GET_DEVICE_INFO, &[], DataPhase::In)?;
    save("device_info.bin", &raw.data)?;
    let info = sony_ptp::DeviceInfo::parse(&raw.data)?;
    println!(
        "{} {} fw {} serial {}  ext 0x{:X} v{} \"{}\"",
        info.manufacturer,
        info.model,
        info.device_version,
        info.serial_number,
        info.vendor_extension_id,
        info.vendor_extension_version,
        info.vendor_extension_desc
    );
    println!(
        "operations: {}",
        info.operations.iter().map(|o| format!("{o:04X}")).collect::<Vec<_>>().join(" ")
    );

    cam.connect().context("Sony SDIO handshake")?;
    let ext = &cam.ext;
    println!(
        "SDIO version {} — {} properties, {} controls",
        ext.version,
        ext.properties.len(),
        ext.controls.len()
    );
    println!(
        "controls: {}",
        ext.controls.iter().map(|o| format!("{o:04X}")).collect::<Vec<_>>().join(" ")
    );
    let ext_json = serde_json::to_string_pretty(&cam.ext)?;

    let all = cam.get_all_props_raw()?;
    save("all_props.bin", &all)?;
    cam.refresh_props()?;
    println!("\n{:<6} {:<26} {:<6} {:<3} {:<3} {:<18} choices", "code", "name", "type", "rw", "en", "value");
    for p in cam.props().values() {
        let choices = p.choices().map_or(String::new(), |c| {
            c.iter().map(|v| format_value(p.code, v)).collect::<Vec<_>>().join(", ")
        });
        let form = match &p.form {
            sony_ptp::Form::Range { min, max, step } => format!(
                "range {}..{} step {}",
                format_value(p.code, min),
                format_value(p.code, max),
                format_value(0, step)
            ),
            _ => choices,
        };
        println!(
            "{:04X}   {:<26} {:04X}   {:<3} {:<3} {:<18} {}",
            p.code,
            codes::prop_name(p.code).unwrap_or("?"),
            p.data_type,
            p.get_set,
            p.is_enabled,
            format_value(p.code, &p.current),
            form
        );
    }

    match cam.liveview_raw() {
        Ok(lv) => {
            save("liveview.bin", &lv)?;
            match extract_jpeg(&lv) {
                Some(j) => {
                    save("liveview.jpg", j)?;
                    println!(
                        "\nlive view OK: object {} bytes, JPEG {} bytes, {:?}",
                        lv.len(),
                        j.len(),
                        jpeg_dimensions(j)
                    );
                }
                None => println!("\nlive view object of {} bytes contains no JPEG", lv.len()),
            }
        }
        Err(e) => println!("\nlive view failed: {e}"),
    }

    if let Some(dir) = &dump {
        std::fs::write(dir.join("ext_device_info.json"), ext_json)?;
        let props: Vec<_> = cam.props().values().collect();
        std::fs::write(dir.join("props.json"), serde_json::to_string_pretty(&props)?)?;
        if let Some(info) = &cam.device_info {
            std::fs::write(dir.join("device_info.json"), serde_json::to_string_pretty(info)?)?;
        }
        println!("dumps written to {}", dir.display());
    }
    Ok(())
}

pub fn liveview(device: Option<String>, frames: usize, out: Option<PathBuf>) -> Result<()> {
    let _com = ComGuard::new()?;
    let (_entry, mut cam) = open(device)?;
    cam.connect()?;
    if let Some(dir) = &out {
        std::fs::create_dir_all(dir)?;
    }
    let start = Instant::now();
    let mut bytes = 0usize;
    let mut failures = 0usize;
    let mut dims = None;
    for i in 0..frames {
        match cam.liveview_jpeg(std::time::Duration::from_secs(2)) {
            Ok(jpeg) => {
                bytes += jpeg.len();
                dims = dims.or(jpeg_dimensions(&jpeg));
                if let Some(dir) = &out {
                    std::fs::write(dir.join(format!("frame_{i:04}.jpg")), &jpeg)?;
                }
            }
            Err(e) => {
                failures += 1;
                eprintln!("frame {i}: {e}");
                if e.is_disconnect() {
                    bail!("camera disconnected");
                }
            }
        }
    }
    let secs = start.elapsed().as_secs_f64();
    let ok = frames - failures;
    println!(
        "{ok}/{frames} frames in {secs:.2}s = {:.1} fps, avg {} KiB, size {:?}",
        ok as f64 / secs,
        bytes / ok.max(1) / 1024,
        dims
    );
    Ok(())
}

fn parse_num(s: &str) -> Result<u32> {
    let s = s.trim();
    // Hexadecimal by default (`0x` optional), decimal with a `d` prefix.
    Ok(match s.strip_prefix('d') {
        Some(d) => d.parse()?,
        None => u32::from_str_radix(s.trim_start_matches("0x").trim_start_matches("0X"), 16)?,
    })
}

fn hexdump(data: &[u8], limit: usize) -> String {
    let mut out = String::new();
    for (i, chunk) in data.chunks(16).take(limit.div_ceil(16)).enumerate() {
        out.push_str(&format!("  {:06X}  ", i * 16));
        for b in chunk {
            out.push_str(&format!("{b:02X} "));
        }
        out.push('\n');
    }
    if data.len() > limit {
        out.push_str(&format!("  ... {} bytes total\n", data.len()));
    }
    out
}

pub fn raw(device: Option<String>, steps: Vec<String>, connect: bool, wait_ms: u64) -> Result<()> {
    let _com = ComGuard::new()?;
    let (_entry, mut cam) = open(device)?;
    if connect {
        cam.connect()?;
    }
    std::thread::sleep(std::time::Duration::from_millis(wait_ms));
    for step in steps {
        let (cmd, phase) = match step.split_once('/') {
            Some((c, p)) => (c, Some(p)),
            None => (step.as_str(), None),
        };
        let mut parts = cmd.split(':');
        let opcode = parse_num(parts.next().context("empty step")?)? as u16;
        let params: Vec<u32> = parts.map(parse_num).collect::<Result<_>>()?;
        let write_buf;
        let phase = match phase {
            None => DataPhase::None,
            Some("r") => DataPhase::In,
            Some(w) if w.starts_with("w=") => {
                let hex = &w[2..];
                write_buf = (0..hex.len())
                    .step_by(2)
                    .map(|i| u8::from_str_radix(&hex[i..i + 2], 16))
                    .collect::<Result<Vec<_>, _>>()?;
                DataPhase::Out(&write_buf)
            }
            Some(other) => bail!("bad phase {other}"),
        };
        let start = Instant::now();
        match cam.transport_mut().execute(opcode, &params, phase) {
            Ok(r) => println!(
                "{step}: 0x{:04X} {} params {:X?} data {} bytes ({} ms)\n{}",
                r.code,
                codes::response_name(r.code),
                r.params,
                r.data.len(),
                start.elapsed().as_millis(),
                hexdump(&r.data, 64)
            ),
            Err(e) => println!("{step}: transport error {e}"),
        }
    }
    Ok(())
}

pub fn set(device: Option<String>, code: &str, value: &str) -> Result<()> {
    let _com = ComGuard::new()?;
    let (_entry, mut cam) = open(device)?;
    cam.connect()?;
    let code = parse_num(code)? as u16;
    let show = |cam: &SonyCamera<WpdTransport>| {
        cam.prop(code).map_or("?".into(), |p| {
            format!("{} (raw 0x{:X}, rw={} en={})", format_value(code, &p.current), p.current_int().unwrap_or(-1), p.get_set, p.is_enabled)
        })
    };
    println!("{} before: {}", codes::prop_name(code).unwrap_or("?"), show(&cam));
    let start = Instant::now();
    let r = match value {
        "up" => cam.step(code, sony_ptp::StepDirection::Up).map(|_| ()),
        "down" => cam.step(code, sony_ptp::StepDirection::Down).map(|_| ()),
        v => {
            let target = match v.strip_prefix("0x") {
                Some(h) => i64::from_str_radix(h, 16)?,
                None => v.parse()?,
            };
            cam.set_value(code, target)
        }
    };
    cam.refresh_props()?;
    println!("after ({} ms): {}  result: {r:?}", start.elapsed().as_millis(), show(&cam));
    Ok(())
}
