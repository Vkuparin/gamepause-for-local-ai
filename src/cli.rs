fn main() {
    if let Err(error) = gamepause_lmstudio::app::main(true) {
        eprintln!("GamePause: {error:#}");
        std::process::exit(1);
    }
}
