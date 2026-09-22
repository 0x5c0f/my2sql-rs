//! my2sql-rs：MySQL binlog 解析 / 还原 SQL 工具（库层根，供 `main` 与集成测试共用）。
//!
//! 层序 (spec §2): `binlog`(协议解码) → `metadata`(表结构) →
//! `pipeline`(源/过滤/事务机/装配) → `sqlopen`(SQL 文本) → `output`(写盘)。

pub mod binlog;
pub mod config;
pub mod flashback;
pub mod metadata;
pub mod output;
pub mod pipeline;
pub mod repl;
pub mod sqlopen;
pub mod stats;

#[cfg(test)]
mod report_integration_tests {
    use crate::flashback::report::{JsonlReporter, SkipEvent};

    #[test]
    fn test_jsonl_reporter_format() {
        // P6 T1: Verify JSONL reporter serializes correctly per D1 spec
        let event = SkipEvent {
            timestamp: "2026-09-22T14:30:15Z".to_string(),
            binlog: "mysql-bin.000150".to_string(),
            position: 12345,
            type_: "Query".to_string(),
            sql: Some("ALTER TABLE t_users ADD COLUMN new_field VARCHAR(100)".to_string()),
        };

        let json = serde_json::to_string(&event).expect("should serialize");

        assert!(json.contains("\"timestamp\":\"2026-09-22T14:30:15Z\""));
        assert!(json.contains("\"binlog\":\"mysql-bin.000150\""));
        assert!(json.contains("\"position\":12345"));
        assert!(json.contains("\"type\":\"Query\""));
        assert!(json.contains("\"sql\":\"ALTER TABLE t_users ADD COLUMN new_field VARCHAR(100\""));
    }
}

