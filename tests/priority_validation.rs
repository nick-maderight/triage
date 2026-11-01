use std::io::Write;
use std::process::{Command, Output, Stdio};

fn assert_priority_rejected(output: Output) {
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("priority must be between 1 and 5"));
}

#[test]
fn finding_and_cloud_event_reject_priorities_outside_one_through_five() {
    for priority in [0, 6] {
        let finding = Command::new(env!("CARGO_BIN_EXE_triage"))
            .args(["finding", "--source", "test://priority", "--title", "Invalid priority", "--priority"])
            .arg(priority.to_string())
            .env("TRIAGE_URL", "http://127.0.0.1:0")
            .output()
            .unwrap();
        assert_priority_rejected(finding);

        let event = serde_json::json!({
            "specversion": "1.0",
            "id": format!("invalid-priority-{priority}"),
            "source": "test://priority",
            "type": "triage.finding",
            "priority": priority,
            "data": {}
        });
        let mut child = Command::new(env!("CARGO_BIN_EXE_triage"))
            .args(["emit", "-"])
            .env("TRIAGE_URL", "http://127.0.0.1:0")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(event.to_string().as_bytes())
            .unwrap();
        assert_priority_rejected(child.wait_with_output().unwrap());
    }
}
