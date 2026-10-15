use super::*;
#[test]
fn content_hash_and_url_encoding_are_stable() {
    assert_eq!(
        native::hash_bytes(b"abc").unwrap(),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(transport::encode("model /?&"), "model%20%2F%3F%26");
}
#[test]
fn current_process_birth_identity_accepts_own_record_and_refuses_reuse() {
    let held = native::Held::open(std::process::id()).unwrap();
    let centiseconds = (held.identity.start_time - 116_444_736_000_000_000 + 50_000) / 100_000;
    assert!(native::Held::recorded(std::process::id(), centiseconds).is_ok());
    assert!(native::Held::recorded(std::process::id(), centiseconds + 100).is_err());
}
#[test]
fn exited_process_with_an_open_handle_is_not_a_live_record() {
    use std::os::windows::process::CommandExt;
    let mut child = std::process::Command::new("cmd.exe")
        .args(["/d", "/c", "exit", "0"])
        .creation_flags(0x08000000)
        .spawn()
        .unwrap();
    child.wait().unwrap();
    // Child retains the process handle after wait, as Hermes may also do.
    assert!(native::Held::open(child.id()).is_ok());
    assert!(
        native::Held::live_recorded(child.id(), 1)
            .unwrap()
            .is_none()
    );
    let held = native::Held::open(std::process::id()).unwrap();
    let birth = (held.identity.start_time - 116_444_736_000_000_000 + 50_000) / 100_000;
    assert!(native::Held::live_recorded(std::process::id(), birth + 100).is_err());
}
#[test]
fn local_http_refuses_remote_routes_redirects_and_oversized_responses() {
    let local = Local::new(r"D:\Fixture\Hermes", "hermes-fixture").unwrap();
    assert!(
        local
            .request(
                "remote:1234",
                "fixture-token",
                "/models",
                None,
                std::time::Duration::from_secs(1)
            )
            .is_err()
    );
    for (status, bytes) in [(302, b"{}".to_vec()), (200, vec![b' '; 1024 * 1024 + 1])] {
        let (address, worker) = crate::ollama_session::tests::http_server(1, move |path, body| {
            assert_eq!(path, "/models");
            assert!(body.is_none());
            (status, bytes.clone())
        });
        assert!(
            local
                .request(
                    &address,
                    "fixture-token",
                    "/models",
                    None,
                    std::time::Duration::from_secs(2)
                )
                .is_err()
        );
        worker.join().unwrap();
    }
}
