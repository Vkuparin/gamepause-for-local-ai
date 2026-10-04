fn main() {
    const {
        assert!(
            cfg!(panic = "unwind"),
            "Production panic containment needs unwinding"
        );
    }
    let folder =
        std::env::temp_dir().join(format!("gamepause-resilience-probe-{}", std::process::id()));
    std::fs::create_dir_all(&folder).unwrap();
    gamepause_lmstudio::app::install_panic_hook(&folder);
    gamepause_lmstudio::discovery::Discovery::default()
        .resilience_probe()
        .unwrap();
    assert!(
        std::fs::read_to_string(folder.join("gamepause.log"))
            .unwrap()
            .contains("injected discovery panic")
    );
    println!("Release panic contained; last-good inventory and next refresh retained");
}
