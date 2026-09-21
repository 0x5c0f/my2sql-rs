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
    // clap 未知参数 → 解析失败出口 2（与 validate 错误同码），stderr 指名该旗标
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--to-stdout"), "{err}");
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
#[test]
fn to_sql_rejects_on_error_stop() {
    // P2 T5 review 裁定：to-sql 恒 best-effort，stop 无消费 → validate 期
    // 拒绝并走 exit(2) 统一错误出口（不静默受理）。
    let out = bin()
        .args([
            "to-sql",
            "--binlog-dir",
            "/tmp",
            "--start-file",
            "f",
            "--uri",
            "mysql://x@y",
            "--on-error",
            "stop",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("stop"), "{err}");
    // P2 T9 措辞收口：旗标只存在于 to-sql/flashback 两处（stats 无 `--on-error`），
    // 故错误串必须点名 **flashback-only**，不得写 "flashback/stats"。
    assert!(err.contains("flashback-only"), "{err}");
    assert!(!err.contains("stats"), "{err}");

    // 顺序钉桩（T9 复审要求入账）：`--on-error stop` 的拒绝发生在 build_common
    // 之前 → 同时缺 schema 源时，用户先看到 on-error 那条，而非 schema 报错。
    let out = bin()
        .args([
            "to-sql",
            "--binlog-dir",
            "/tmp",
            "--start-file",
            "f",
            "--on-error",
            "stop",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("flashback-only"), "{err}");
    assert!(
        !err.contains("schema source"),
        "--on-error 拒绝应早于 build_common: {err}"
    );
}
