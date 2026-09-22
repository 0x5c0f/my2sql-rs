//! JSONL reporter for DDL skip events (P6 T1 Deliverable 2)
//!
//! Per spec D1: Minimal JSONL summary format for audit trail
//! Only reports skips - does NOT generate reverse SQL (out of scope)

use serde::Serialize;
use std::fs::File;
use std::io::{BufWriter, Write};

#[derive(Serialize)]
pub struct SkipEvent {
    pub timestamp: String,
    pub binlog: String,
    pub position: u64,
    #[serde(rename = "type")]
    pub type_: String,
    pub sql: Option<String>,
}

pub struct JsonlReporter {
    writer: BufWriter<File>,
}

impl JsonlReporter {
    pub fn new(path: &str) -> Result<Self, std::io::Error> {
        let file = File::create(path)?;
        Ok(Self {
            writer: BufWriter::new(file),
        })
    }

    pub fn write(&mut self, event: &SkipEvent) -> Result<(), std::io::Error> {
        writeln!(self.writer, "{}", serde_json::to_string(event).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
        })?)?;
        Ok(())
    }

    pub fn close(&mut self) -> Result<(), std::io::Error> {
        self.writer.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_test_skip_event_serialization() {
        let event = SkipEvent {
            timestamp: "2026-09-22T14:30:15Z".to_string(),
            binlog: "mysql-bin.000150".to_string(),
            position: 12345,
            type_: "Query".to_string(),
            sql: Some("ALTER TABLE t_users ADD COLUMN new_field VARCHAR(100)".to_string()),
        };

        let json = serde_json::to_string(&event).expect("should serialize");

        // Verify all required fields present
        assert!(json.contains("\"timestamp\":\"2026-09-22T14:30:15Z\""));
        assert!(json.contains("\"binlog\":\"mysql-bin.000150\""));
        assert!(json.contains("\"position\":12345"));
        assert!(json.contains("\"type\":\"Query\""));
        assert!(json.contains("\"sql\":\"ALTER TABLE t_users ADD COLUMN new_field VARCHAR(100\""));
    }

    #[test]
    fn test_skip_event_with_null_sql() {
        let event = SkipEvent {
            timestamp: "2026-09-22T14:31:22Z".to_string(),
            binlog: "mysql-bin.000150".to_string(),
            position: 67890,
            type_: "Rows".to_string(),
            sql: None,
        };

        let json = serde_json::to_string(&event).expect("should serialize");

        assert!(json.contains("\"sql\":null"));
    }

    #[test]
    fn test_jsonl_writer() -> Result<(), Box<dyn std::error::Error>> {
        let test_dir = std::env::temp_dir().join("my2sql_test_report");
        let _ = std::fs::remove_dir_all(&test_dir); // cleanup
        std::fs::create_dir_all(&test_dir)?;
        let output_file = test_dir.join("report.jsonl");

        let mut writer = JsonlReporter::new(output_file.to_str().unwrap())?;

        let event = SkipEvent {
            timestamp: "2026-09-22T14:30:15Z".to_string(),
            binlog: "mysql-bin.000150".to_string(),
            position: 12345,
            type_: "Query".to_string(),
            sql: Some("ALTER TABLE t_users ADD COLUMN new_field VARCHAR(100)".to_string()),
        };

        writer.write(&event)?;
        writer.close()?;

        let content = std::fs::read_to_string(&output_file)?;
        let lines: Vec<&str> = content.lines().collect();

        assert_eq!(lines.len(), 1, "Should have exactly one line");
        assert!(lines[0].contains("\"binlog\": \"mysql-bin.000150\""));
        assert!(lines[0].contains("\"position\": 12345"));

        std::fs::remove_dir_all(&test_dir)?;
        Ok(())
    }

    #[test]
    fn test_jsonl_multiple_events() -> Result<(), Box<dyn std::error::Error>> {
        let test_dir = std::env::temp_dir().join("my2sql_test_multi");
        let _ = std::fs::remove_dir_all(&test_dir);
        std::fs::create_dir_all(&test_dir)?;
        let output_file = test_dir.join("report_multi.jsonl");

        let mut writer = JsonlReporter::new(output_file.to_str().unwrap())?;

        let events = vec![
            SkipEvent {
                timestamp: "2026-09-22T14:30:15Z".to_string(),
                binlog: "mysql-bin.000150".to_string(),
                position: 12345,
                type_: "Query".to_string(),
                sql: Some("ALTER TABLE t1 ADD col1 INT".to_string()),
            },
            SkipEvent {
                timestamp: "2026-09-22T14:31:22Z".to_string(),
                binlog: "mysql-bin.000150".to_string(),
                position: 67890,
                type_: "Rows".to_string(),
                sql: None,
            },
        ];

        for event in &events {
            writer.write(event)?;
        }
        writer.close()?;

        let content = std::fs::read_to_string(&output_file)?;
        let lines: Vec<&str> = content.lines().collect();

        assert_eq!(lines.len(), 2, "Should have two lines");
        assert!(lines[0].contains("\"type\":\"Query\""));
        assert!(lines[1].contains("\"type\":\"Rows\""));

        std::fs::remove_dir_all(&test_dir)?;
        Ok(())
    }
}
