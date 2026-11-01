use std::{path::PathBuf, process::Command};

#[test]
fn ui_checks_do_not_leak_long_title_check() {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/ui-checks.js");
    let status = Command::new("node")
        .arg("-e")
        .arg(
            "const fs = require('node:fs'); const vm = require('node:vm'); const context = {}; vm.runInNewContext(fs.readFileSync(process.argv[1], 'utf8'), context); if (Object.hasOwn(context, 'checkLongTitle')) process.exit(1); if (typeof context.checkTriage !== 'function') process.exit(2);",
        )
        .arg(script)
        .status()
        .expect("node must be available to validate the browser check script");
    assert!(status.success(), "browser checks must not leak internal functions");
}
