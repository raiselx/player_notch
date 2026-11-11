#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(windows)]
mod app;
#[cfg(windows)]
mod apple_music;
#[cfg(windows)]
mod artwork;
#[cfg(windows)]
mod media;
mod model;
#[cfg(windows)]
mod now_playing;
#[cfg(windows)]
mod platform;
#[cfg(windows)]
mod settings;
#[cfg(windows)]
mod settings_ui;
#[cfg(windows)]
mod theme;

#[cfg(windows)]
fn main() {
    install_crash_log();
    if let Err(error) = run() {
        eprintln!("PlayerNotch: {error}");
        append_log(&error);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("PlayerNotch requires Windows 10/11 and Windows SMTC.");
}

#[cfg(windows)]
fn run() -> Result<(), String> {
    // Give the taskbar, Start Menu shortcut and notifications a stable application identity.
    unsafe {
        let _ = windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID(
            windows::core::w!("PlayerNotch.App"),
        );
    }
    let args: Vec<_> = std::env::args().collect();
    if args
        .iter()
        .any(|arg| arg == "--enable-autostart" || arg == "--disable-autostart")
    {
        platform::set_autostart(args.iter().any(|arg| arg == "--enable-autostart"))?;
        println!("autostart: {}", platform::autostart_enabled());
        return Ok(());
    }
    let demo = args.iter().any(|arg| arg == "--demo");
    let autostart = args.iter().any(|arg| arg == "--autostart");
    let open_settings = args.iter().any(|arg| arg == "--settings");
    let _instance = if demo {
        None
    } else {
        match platform::InstanceGuard::acquire()? {
            Some(guard) => Some(guard),
            None => return Ok(()),
        }
    };
    if !demo {
        if let Err(error) = platform::ensure_start_menu_shortcut() {
            append_log(&format!("Start Menu shortcut: {error}"));
        }
    }
    let desktop = platform::desktop_info();
    let mini_size = app::MINI_SIZE;
    let icon = image::load_from_memory(include_bytes!("../PlayerNotch.ico"))
        .map(|im| {
            let rgba = im.into_rgba8();
            eframe::egui::IconData {
                width: rgba.width(),
                height: rgba.height(),
                rgba: rgba.into_raw(),
            }
        })
        .map_err(|e| e.to_string())?;
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("PlayerNotch")
            .with_inner_size(mini_size)
            .with_position([
                desktop.origin[0] + (desktop.size[0] - mini_size[0]) / 2.0,
                desktop.origin[1],
            ])
            .with_decorations(false)
            .with_resizable(false)
            .with_transparent(true)
            .with_taskbar(false)
            .with_active(false)
            .with_always_on_top()
            .with_icon(icon),
        // The overlay schedules its own frames and Windows composites the result.
        vsync: false,
        ..Default::default()
    };
    eframe::run_native(
        "PlayerNotch",
        options,
        Box::new(move |cc| {
            Ok(Box::new(app::NotchApp::new(
                cc,
                demo,
                autostart,
                open_settings,
            )))
        }),
    )
    .map_err(|e| e.to_string())
}

#[cfg(windows)]
fn install_crash_log() {
    std::panic::set_hook(Box::new(|info| append_log(&info.to_string())));
}

#[cfg(windows)]
fn append_log(text: &str) {
    use std::io::Write;
    let dir = std::env::var_os("APPDATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("PlayerNotch");
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("crash.log"))
    {
        let _ = writeln!(file, "{:?}: {text}", std::time::SystemTime::now());
    }
}
