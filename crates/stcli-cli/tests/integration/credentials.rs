use stcli_testkit::TestHome;
use std::process::Command;

fn stcli(home: &TestHome) -> Command {
    let mut command = Command::new(home.stcli_binary());
    command.env("STCLI_HOME", home.root());
    command
}

#[test]
fn credentials_command_exposes_set_list_and_delete() {
    let home = TestHome::new().unwrap();
    let output = stcli(&home)
        .args(["credentials", "--help"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(help.contains("set"));
    assert!(help.contains("list"));
    assert!(help.contains("delete"));

    let invalid = stcli(&home)
        .args(["credentials", "delete"])
        .output()
        .unwrap();
    assert!(!invalid.status.success());
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("<ALIAS>"));
}
