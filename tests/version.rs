use std::process::Command;

fn run(bin: &str, arg: &str) -> String {
    let out = Command::new(bin).arg(arg).output().expect("run binary");
    assert!(out.status.success(), "{arg} should exit 0");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn version_flags_print_the_crate_version() {
    let bin = env!("CARGO_BIN_EXE_codetypera2a");
    let expected = format!("codetypera2a {}", env!("CARGO_PKG_VERSION"));
    for arg in ["--version", "-V", "version"] {
        assert_eq!(run(bin, arg), expected);
    }
}

#[test]
fn oc_version_prints_the_crate_version() {
    let bin = env!("CARGO_BIN_EXE_codetypera2a");
    let out = Command::new(bin).args(["oc", "--version"]).output().expect("run oc");
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(text.trim(), format!("oc {}", env!("CARGO_PKG_VERSION")));
}
