#![windows_subsystem = "windows"]
fn main() {
    if let Err(error) = gamepause::app::main(false) {
        gamepause::tray::error(&format!("{error:#}"));
    }
}
