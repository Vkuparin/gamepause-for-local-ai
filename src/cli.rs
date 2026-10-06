fn main() {
    if let Err(error) = gamepause::app::main(true) {
        eprintln!("GamePause: {error:#}");
        std::process::exit(1);
    }
}
