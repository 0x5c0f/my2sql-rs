use std::process::Command;
fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_my2sql-rs"))
}

#[test]
fn help_lists_subcommands() {
    let out = bin().arg("--help").output().unwrap();
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("to-sql"), "{s}");
    assert!(s.contains("flashback"), "{s}");
    assert!(s.contains("stats"), "{s}");
}
#[test]
fn flashback_help_shows_keep_trx_and_rejects_to_stdout() {
    let out = bin().args(["flashback", "--help"]).output().unwrap();
    assert!(out.status.success());
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("--keep-trx"), "{s}");
    assert!(s.contains("--no-keep-trx"), "{s}");
    assert!(s.contains("--on-error"), "{s}");
    // T3 登记边界：flashback 无 --to-stdout（逆序回写需要盘上文件）
    assert!(!s.contains("--to-stdout"), "{s}");
    let out = bin()
        .args([
            "flashback",
            "--binlog-dir",
            "/tmp",
            "--start-file",
            "f",
            "--to-stdout",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
}
#[test]
fn stats_help_shows_threshold_flags() {
    let out = bin().args(["stats", "--help"]).output().unwrap();
    assert!(out.status.success());
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("--print-interval"), "{s}");
    assert!(s.contains("--big-trx-rows"), "{s}");
    assert!(s.contains("--long-trx-seconds"), "{s}");
    assert!(s.contains("--stats-json"), "{s}");
    // stats 无 SQL 文本旗标面（无产物）
    assert!(!s.contains("--full-columns"), "{s}");
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
