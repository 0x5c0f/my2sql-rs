use std::process::Command;
fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_my2sql-rs"))
}

#[test]
fn help_lists_subcommands() {
    let out = bin().arg("--help").output().unwrap();
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("to-sql"), "{s}");
}
#[test]
fn to_sql_requires_start_file() {
    let out = bin()
        .args(["to-sql", "--binlog-dir", "/tmp", "--output-dir", "/tmp"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("start-file"));
}
#[test]
fn bad_dml_value_rejected() {
    let out = bin()
        .args([
            "to-sql",
            "--binlog-dir",
            "/tmp",
            "--start-file",
            "mysql-bin.000001",
            "--dml",
            "truncate",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
}
