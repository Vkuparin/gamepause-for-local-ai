fn main() {
    println!("cargo:rerun-if-changed=assets/gamepause.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winresource::WindowsResource::new()
            .set_icon("assets/gamepause.ico")
            .set("ProductName", "GamePause for LM Studio")
            .set("FileDescription", "GamePause for LM Studio")
            .set("CompanyName", "GamePause contributors")
            .set(
                "LegalCopyright",
                "Copyright (c) 2026 GamePause contributors",
            )
            .compile()
            .expect("Windows executable resource compilation failed");
    }
}
