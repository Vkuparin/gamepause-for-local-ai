#![windows_subsystem = "windows"]
fn main() {
    if let Err(error) = gamepause_lmstudio::app::main(false) {
        gamepause_lmstudio::tray::error(&format!("{error:#}"));
    }
}
