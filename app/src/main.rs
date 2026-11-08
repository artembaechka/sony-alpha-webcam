mod cli;
mod gui;
mod vcam;
mod worker;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "sony-remote", version, about = "Live view and remote control for Sony Alpha cameras over USB")]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,
    /// Log filter, e.g. `debug` or `sony_ptp=trace`.
    #[arg(long, global = true, default_value = "info")]
    log: String,
}

#[derive(Subcommand)]
enum Command {
    /// List portable devices and mark the Sony ones.
    List,
    /// Connect, print device info and all properties.
    Probe {
        /// Directory to write raw dumps and JSON into.
        #[arg(long)]
        dump: Option<std::path::PathBuf>,
        /// PnP device id (defaults to the first Sony camera).
        #[arg(long)]
        device: Option<String>,
    },
    /// Grab live view frames, save them as JPEG and report the frame rate.
    Liveview {
        #[arg(long, default_value_t = 60)]
        frames: usize,
        #[arg(long)]
        out: Option<std::path::PathBuf>,
        #[arg(long)]
        device: Option<String>,
    },
    /// Change a property: `set D20D up`, `set 5010 down`, `set 5005 0x8011`.
    Set {
        /// Property code (hex).
        code: String,
        /// `up`, `down` or a target value (hex with 0x, otherwise decimal).
        #[arg(allow_hyphen_values = true)]
        value: String,
        #[arg(long)]
        device: Option<String>,
    },
    /// Run raw PTP transactions after the Sony handshake (developer tool).
    ///
    /// Each step is `OP[:P1[:P2...]][/r|/w=HEX]`, e.g. `1008:0xFFFFC002/r`.
    Raw {
        steps: Vec<String>,
        /// Skip the SDIO handshake.
        #[arg(long)]
        no_connect: bool,
        /// Wait this many milliseconds after connecting.
        #[arg(long, default_value_t = 0)]
        wait: u64,
        #[arg(long)]
        device: Option<String>,
    },
    /// Run without a window: keep the camera connected and feed the virtual webcam.
    Serve,
    /// Manage the virtual webcam (install needs administrator rights once).
    Vcam {
        #[command(subcommand)]
        action: VcamAction,
    },
}

#[derive(Subcommand)]
enum VcamAction {
    /// Install the media source DLL and register the virtual camera.
    Install {
        #[arg(long, hide = true)]
        report: Option<std::path::PathBuf>,
    },
    /// Remove the virtual camera and its DLL.
    Uninstall {
        #[arg(long, hide = true)]
        report: Option<std::path::PathBuf>,
    },
    /// Show registration and streaming state.
    Status,
    /// Load the media source DLL in-process and pull samples (no install).
    #[command(hide = true)]
    Selftest,
    /// Read frames from the installed virtual camera like a video app would.
    #[command(hide = true)]
    Consume {
        #[arg(long, default_value_t = 60)]
        frames: usize,
        /// Save the last frame as BMP.
        #[arg(long)]
        bmp: Option<std::path::PathBuf>,
    },
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_new(&args.log)?)
        .with_writer(std::io::stderr)
        .init();

    match args.command {
        Some(Command::List) => cli::list(),
        Some(Command::Probe { dump, device }) => cli::probe(device, dump),
        Some(Command::Liveview { frames, out, device }) => cli::liveview(device, frames, out),
        Some(Command::Set { code, value, device }) => cli::set(device, &code, &value),
        Some(Command::Raw { steps, no_connect, wait, device }) => cli::raw(device, steps, !no_connect, wait),
        Some(Command::Serve) => serve(),
        Some(Command::Vcam { action }) => match action {
            VcamAction::Install { report } => vcam::install(report),
            VcamAction::Uninstall { report } => vcam::uninstall(report),
            VcamAction::Status => vcam::status(),
            VcamAction::Selftest => vcam::selftest(),
            VcamAction::Consume { frames, bmp } => vcam::consume(frames, bmp),
        },
        None => {
            hide_own_console();
            gui::run()
        }
    }
}

/// When started from Explorer the console window belongs to us alone: get
/// rid of it. When started from a terminal, keep logging there.
fn hide_own_console() {
    use windows::Win32::System::Console::{FreeConsole, GetConsoleProcessList};
    let mut pids = [0u32; 2];
    if unsafe { GetConsoleProcessList(&mut pids) } == 1 {
        let _ = unsafe { FreeConsole() };
    }
}

/// Headless mode: camera connection + virtual webcam, status on stdout.
fn serve() -> anyhow::Result<()> {
    let (writer, vcam_status) = vcam::writer();
    let worker = worker::Worker::spawn(vec![writer]);
    let mut last = String::new();
    loop {
        let status = worker.shared.status.lock().unwrap().clone();
        let fps = *worker.shared.fps.lock().unwrap();
        let line = format!("{status:?} | webcam {:?}", vcam_status.get());
        if line != last {
            println!("{line}");
            last = line;
        }
        tracing::debug!("{fps:.1} fps");
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}
