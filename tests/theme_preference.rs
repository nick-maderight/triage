use std::process::Command;

#[test]
fn theme_preference() {
    let status = Command::new("node")
        .args(["--test", "tests/theme-preference.test.js"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .status()
        .expect("Node.js is required to run the theme preference regression test");

    assert!(status.success(), "theme preference regression test failed");
}
