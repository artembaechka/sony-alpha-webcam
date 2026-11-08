//! egui front end: live view plus exposure and focus controls.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use eframe::egui::{self, Color32, ColorImage, RichText, TextureHandle, TextureOptions};
use sony_ptp::codes::{self, prop};
use sony_ptp::format::format_value;
use sony_ptp::{PropDesc, StepDirection, Value};

use crate::vcam::{self, VcamState, VcamStatus};
use crate::worker::{CameraState, Command, Status, Worker};

pub fn run() -> anyhow::Result<()> {
    let (vcam_writer, vcam_status) = vcam::writer();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Sony Remote")
            .with_inner_size([1340.0, 720.0])
            .with_min_inner_size([640.0, 400.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Sony Remote",
        options,
        Box::new(move |cc| {
            let ctx = cc.egui_ctx.clone();
            let listeners: Vec<crate::worker::FrameListener> =
                vec![vcam_writer, Box::new(move |_frame| ctx.request_repaint())];
            Ok(Box::new(App::new(Worker::spawn(listeners), vcam_status)))
        }),
    )
    .map_err(|e| anyhow::anyhow!("GUI failed: {e}"))
}

struct App {
    worker: Worker,
    vcam: Arc<VcamStatus>,
    vcam_busy: Arc<std::sync::atomic::AtomicBool>,
    texture: Option<TextureHandle>,
    shown_seq: u64,
    frame_size: [usize; 2],
    af_down: bool,
}

impl App {
    fn new(worker: Worker, vcam: Arc<VcamStatus>) -> Self {
        Self {
            worker,
            vcam,
            vcam_busy: Default::default(),
            texture: None,
            shown_seq: 0,
            frame_size: [0, 0],
            af_down: false,
        }
    }

    fn vcam_ui(&mut self, ui: &mut egui::Ui) {
        let busy = self.vcam_busy.load(Ordering::Relaxed);
        match self.vcam.get() {
            VcamState::Streaming => {
                ui.label(RichText::new("● Webcam in use").color(Color32::from_rgb(210, 80, 70)));
            }
            VcamState::Idle => {
                ui.label(RichText::new("Webcam ready").color(Color32::GRAY))
                    .on_hover_text(format!("Select \"{}\" in Zoom, OBS, Teams or the browser", vcam_shared::FRIENDLY_NAME));
            }
            VcamState::NotInstalled => {
                let resp = ui
                    .add_enabled(!busy, egui::Button::new("Install webcam"))
                    .on_hover_text("Registers the virtual camera (asks for administrator rights once)");
                if resp.clicked() {
                    let flag = self.vcam_busy.clone();
                    let status = self.vcam.clone();
                    flag.store(true, Ordering::Relaxed);
                    std::thread::spawn(move || {
                        let r = vcam::install(None);
                        if let Err(e) = &r {
                            tracing::warn!("virtual camera install failed: {e:#}");
                        }
                        if vcam::is_installed() {
                            status.mark_installed();
                        }
                        flag.store(false, Ordering::Relaxed);
                    });
                }
                if busy {
                    ui.spinner();
                }
            }
        }
    }

    fn update_texture(&mut self, ctx: &egui::Context) {
        let frame = self.worker.shared.frame.lock().unwrap().clone();
        let Some(frame) = frame else { return };
        if frame.seq == self.shown_seq {
            return;
        }
        self.shown_seq = frame.seq;
        self.frame_size = [frame.width, frame.height];
        let image = ColorImage::from_rgba_unmultiplied([frame.width, frame.height], &frame.rgba);
        match &mut self.texture {
            Some(t) => t.set(image, TextureOptions::LINEAR),
            None => self.texture = Some(ctx.load_texture("liveview", image, TextureOptions::LINEAR)),
        }
    }
}

const LABEL_WIDTH: f32 = 96.0;

/// Exposure modes in which the camera decides exposure by itself.
fn is_auto_mode(mode: i64) -> bool {
    !matches!(mode, 1..=4 | 0x8050..=0x8053 | 0x8084..=0x8087)
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.update_texture(ctx);
        let shared = self.worker.shared.clone();
        let status = shared.status.lock().unwrap().clone();
        let cam: Arc<CameraState> = shared.camera.lock().unwrap().clone();
        let fps = *shared.fps.lock().unwrap();
        let busy = shared.busy.load(Ordering::Relaxed);
        let connected = matches!(status, Status::Connected(_));

        egui::TopBottomPanel::top("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                let (color, text) = match &status {
                    Status::Connected(m) => (Color32::from_rgb(80, 180, 90), format!("● {m}")),
                    Status::Connecting(m) => (Color32::from_rgb(220, 170, 50), format!("◌ Connecting to {m}…")),
                    Status::Searching { .. } => (Color32::GRAY, "○ Waiting for camera…".to_owned()),
                    Status::WrongMode(_) => (Color32::from_rgb(220, 170, 50), "○ Wrong USB mode".to_owned()),
                    Status::Error(_) => (Color32::from_rgb(210, 80, 70), "✖ Connection lost, retrying…".to_owned()),
                };
                ui.label(RichText::new(text).color(color).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    self.vcam_ui(ui);
                    ui.separator();
                    if connected {
                        if let Some(b) = cam.props.get(&prop::SONY_BATTERY_LEVEL) {
                            ui.label(format!("Battery {}", format_value(b.code, &b.current)));
                            ui.separator();
                        }
                        ui.label(format!("{:.0} fps  {}×{}", fps, self.frame_size[0], self.frame_size[1]));
                        if busy {
                            ui.spinner();
                        }
                    }
                });
            });
        });

        let notice = shared.notice.lock().unwrap().clone();
        let detail = match &status {
            Status::Searching { hint } => Some(hint.clone().unwrap_or_else(|| {
                "Turn the camera on, set MENU → Setup → USB Connection → PC Remote and connect it via USB.".to_owned()
            })),
            Status::WrongMode(msg) | Status::Error(msg) => Some(msg.clone()),
            _ => notice
                .filter(|(t, _)| t.elapsed() < Duration::from_secs(6))
                .map(|(_, m)| m),
        };
        if let Some(detail) = detail {
            egui::TopBottomPanel::bottom("detail").show(ctx, |ui| {
                ui.label(RichText::new(detail).color(Color32::from_rgb(220, 170, 50)));
            });
        }

        egui::SidePanel::right("controls")
            .resizable(false)
            .exact_width(330.0)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.add_enabled_ui(connected, |ui| self.controls(ui, &cam, busy));
                });
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(Color32::from_gray(16)))
            .show(ctx, |ui| {
                let avail = ui.available_size();
                match &self.texture {
                    Some(tex) if connected => {
                        let [w, h] = self.frame_size;
                        let scale = (avail.x / w as f32).min(avail.y / h as f32);
                        let size = egui::vec2(w as f32 * scale, h as f32 * scale);
                        ui.centered_and_justified(|ui| {
                            ui.add(egui::Image::new((tex.id(), size)).fit_to_exact_size(size));
                        });
                    }
                    _ => {
                        ui.centered_and_justified(|ui| {
                            ui.label(RichText::new("No live view").color(Color32::GRAY).size(20.0));
                        });
                    }
                }
            });

        // Keep the status line fresh even without frames.
        ctx.request_repaint_after(Duration::from_millis(250));
    }
}

impl App {
    fn controls(&mut self, ui: &mut egui::Ui, cam: &CameraState, busy: bool) {
        ui.add_space(6.0);
        ui.heading("Exposure");
        ui.add_space(4.0);

        let mode = cam.props.get(&prop::EXPOSURE_PROGRAM_MODE);
        if let Some(m) = mode {
            ui.horizontal(|ui| {
                ui.add_sized([LABEL_WIDTH, 18.0], egui::Label::new("Mode"));
                ui.label(RichText::new(format_value(m.code, &m.current)).strong());
            });
        }

        egui::Grid::new("exposure").num_columns(2).spacing([6.0, 6.0]).show(ui, |ui| {
            for code in [
                prop::F_NUMBER,
                prop::SONY_SHUTTER_SPEED,
                prop::SONY_ISO,
                prop::EXPOSURE_BIAS,
                prop::WHITE_BALANCE,
                prop::SONY_COLOR_TEMP,
            ] {
                self.prop_row(ui, cam, code, busy);
            }
        });

        if let Some(m) = mode.and_then(PropDesc::current_int) {
            if is_auto_mode(m) {
                ui.add_space(4.0);
                ui.label(
                    RichText::new("Exposure is automatic in this mode. Turn the mode dial to P, A, S or M to change it.")
                        .small()
                        .color(Color32::GRAY),
                );
            }
        }

        ui.add_space(12.0);
        ui.separator();
        ui.heading("Focus");
        ui.add_space(4.0);
        egui::Grid::new("focus").num_columns(2).spacing([6.0, 6.0]).show(ui, |ui| {
            self.prop_row(ui, cam, prop::FOCUS_MODE, busy);
            if let Some(f) = cam.props.get(&prop::SONY_FOCUS_FOUND) {
                ui.label("Status");
                let text = format_value(f.code, &f.current);
                let color = match f.current_int() {
                    Some(2) => Color32::from_rgb(80, 180, 90),
                    Some(3) => Color32::from_rgb(210, 80, 70),
                    _ => Color32::GRAY,
                };
                ui.label(RichText::new(text).color(color));
                ui.end_row();
            }
        });
        ui.add_space(6.0);

        if cam.supported.contains(&prop::SONY_S1) {
            let resp = ui
                .add_sized([300.0, 32.0], egui::Button::new("Autofocus (hold)"))
                .on_hover_text("Half-presses the shutter button while held");
            let down = resp.is_pointer_button_down_on();
            if down != self.af_down {
                self.af_down = down;
                self.worker.send(Command::HalfPress(down));
            }
        }

        if cam.supported.contains(&prop::SONY_NEAR_FAR) {
            ui.add_space(6.0);
            ui.label("Manual focus");
            ui.horizontal(|ui| {
                for (label, step) in [("⏪ Near", -7), ("◀", -3), ("‹", -1), ("›", 1), ("▶", 3), ("Far ⏩", 7)] {
                    if ui.button(label).clicked() {
                        self.worker.send(Command::FocusStep(step));
                    }
                }
            });
        } else {
            ui.add_space(4.0);
            ui.label(
                RichText::new("This camera does not offer remote manual focus; use the focus ring.")
                    .small()
                    .color(Color32::GRAY),
            );
        }

        ui.add_space(12.0);
        ui.separator();
        egui::CollapsingHeader::new("All properties").default_open(false).show(ui, |ui| {
            egui::Grid::new("all").num_columns(2).striped(true).show(ui, |ui| {
                for p in cam.props.values() {
                    let name = codes::prop_name(p.code).map_or_else(|| format!("0x{:04X}", p.code), str::to_owned);
                    ui.label(RichText::new(name).small());
                    let mut v = format_value(p.code, &p.current);
                    if p.adjustable() {
                        v.push_str("  ✎");
                    }
                    ui.label(RichText::new(v).small());
                    ui.end_row();
                }
            });
        });
    }

    fn prop_row(&mut self, ui: &mut egui::Ui, cam: &CameraState, code: u16, busy: bool) {
        let Some(p) = cam.props.get(&code) else { return };
        ui.add_sized([LABEL_WIDTH, 20.0], egui::Label::new(codes::prop_name(code).unwrap_or("?")));
        ui.horizontal(|ui| {
            let editable = p.adjustable();
            let current = format_value(code, &p.current);
            // Properties that move in steps get ◀ ▶ buttons.
            let steppable = p.steppable();
            ui.add_enabled_ui(editable && !busy, |ui| {
                if steppable && ui.small_button("◀").clicked() {
                    self.worker.send(Command::Step { code, dir: StepDirection::Down });
                }
                let choices: Vec<&Value> = p
                    .choices()
                    .map(|c| {
                        c.iter()
                            .filter(|v| code != prop::SONY_ISO || v.as_int().is_some_and(|i| i >> 24 == 0) || **v == p.current)
                            .collect()
                    })
                    .unwrap_or_default();
                if editable && !choices.is_empty() {
                    egui::ComboBox::from_id_salt(("prop", code))
                        .width(150.0)
                        .selected_text(&current)
                        .height(400.0)
                        .show_ui(ui, |ui| {
                            for v in choices {
                                if ui.selectable_label(*v == p.current, format_value(code, v)).clicked() {
                                    if let Some(value) = v.as_int() {
                                        self.worker.send(Command::SetValue { code, value });
                                    }
                                }
                            }
                        });
                } else {
                    ui.add_sized([150.0, 20.0], egui::Label::new(RichText::new(&current).strong()));
                }
                if steppable && ui.small_button("▶").clicked() {
                    self.worker.send(Command::Step { code, dir: StepDirection::Up });
                }
            });
        });
        ui.end_row();
    }
}
