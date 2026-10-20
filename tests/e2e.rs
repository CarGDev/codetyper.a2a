#[test]
fn hub_end_to_end() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_codetypera2a"))
        .arg("e2e-test")
        .output()
        .expect("run e2e-test");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "e2e failed:\n{text}");
    assert!(text.contains("ALL PASSED"), "{text}");
}
