#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(windows)]
fn main() {
    if let Err(error) = gemini_cdp_cli::desktop::run() {
        let message = format!("EVIL GEMINI: {error:#}");
        eprintln!("{message}");
        let path = std::env::temp_dir().join("evil-gemini-startup-error.txt");
        let _ = std::fs::write(&path, message);
        let _ = std::process::Command::new("notepad.exe").arg(path).spawn();
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("EVIL GEMINI desktop currently supports Windows.");
}
