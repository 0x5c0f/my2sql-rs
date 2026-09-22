//! Transaction counting for dry-run mode (P6-T2)
//!
//! Counts transactions without generating SQL files - provides summary statistics only.

use serde::Serialize;
use std::collections::HashMap;

/// Binlog range information for summary output
#[derive(Serialize)]
pub struct BinlogRange {
    pub start_file: String,
    pub start_pos: u32,
    pub end_file: String,
    pub end_pos: u32,
}

/// Statistics about scanned transactions
#[derive(Serialize)]
pub struct Stats {
    pub total_transactions: usize,
    pub recoverable_transactions: usize,
    pub skipped_events: usize,
    pub estimated_rows_affected: usize,
    pub recovery_rate: String,
}

/// Warning category with count
#[derive(Serialize)]
pub struct Warning {
    pub count: usize,
    pub type_: String,
}

/// Complete dry-run summary (matches spec D2 format)
#[derive(Serialize)]
pub struct Summary {
    pub binlog_range: BinlogRange,
    pub summary: Stats,
    pub warnings: Vec<Warning>,
}

/// State machine for counting transactions during dry-run
pub struct TransactionCounter {
    total_trx: usize,
    recoverable_trx: usize,
    skipped_events: usize,
    rows_affected: usize,
    warnings: HashMap<String, usize>,
    current_binlog: String,
    current_start_pos: u32,
    last_end_pos: u32,
}

impl TransactionCounter {
    pub fn new() -> Self {
        Self {
            total_trx: 0,
            recoverable_trx: 0,
            skipped_events: 0,
            rows_affected: 0,
            warnings: HashMap::new(),
            current_binlog: String::new(),
            current_start_pos: 0,
            last_end_pos: 0,
        }
    }

    /// Increment warning count by type
    pub fn add_warning(&mut self, warning_type: &str) {
        *self.warnings.entry(warning_type.to_string()).or_insert(0) += 1;
    }

    /// Count a single RawEvent from the pipeline source
    pub fn count_event(&mut self, event: crate::pipeline::source::RawEvent) {
        // Track binlog range (always do this)
        if self.current_binlog.is_empty() {
            self.current_binlog = event.binlog.clone();
            self.current_start_pos = event.start_pos;
        }
        self.last_end_pos = event.end_pos;

        // Then match on kind for counting
        match &event.kind {
            // BEGIN - start of transaction
            crate::pipeline::source::RawKind::Query(sql) if sql.to_uppercase() == "BEGIN" => {
                self.total_trx += 1;
                self.recoverable_trx += 1;
            }

            // XID - commit point for autocommit transactions
            crate::pipeline::source::RawKind::Xid => {
                self.total_trx += 1;
                self.recoverable_trx += 1;
            }

            // Rows - DML operations, count affected rows
            crate::pipeline::source::RawKind::Rows(_, _) => {
                // Approximate row count from body (simplified)
                self.rows_affected += 1;
            }

            // ROTATE - track binlog file change (already handled above for range)
            crate::pipeline::source::RawKind::Rotate(next_file) => {
                if !next_file.is_empty() {
                    self.current_binlog = next_file.clone();
                }
            }

            // DDL Query events - count as skipped
            crate::pipeline::source::RawKind::Query(sql) if is_ddl_query(sql) => {
                self.skipped_events += 1;
                self.add_warning("query_event");
            }

            // Gtid, Other - no special handling
            _ => {}
        }
    }

    /// Track binlog file name during iteration (for range summary)
    pub fn set_current_binlog_name(&mut self, name: &str) {
        if self.current_binlog.is_empty() {
            self.current_binlog = name.to_string();
        }
    }

    /// Build final summary with calculated metrics
    pub fn build_summary(self) -> Summary {
        let recovery_rate = if self.total_trx == 0 {
            "0.00%".to_string()
        } else {
            format!(
                "{:.2}%",
                self.recoverable_trx as f64 / self.total_trx as f64 * 100.0
            )
        };

        let mut warnings: Vec<Warning> = self
            .warnings
            .into_iter()
            .map(|(type_, count)| Warning { count, type_ })
            .collect();

        // Sort warnings by count descending for better visibility
        warnings.sort_by_key(|b| std::cmp::Reverse(b.count));

        Summary {
            binlog_range: BinlogRange {
                start_file: self.current_binlog.clone(),
                start_pos: self.current_start_pos,
                end_file: self.current_binlog,
                end_pos: self.last_end_pos,
            },
            summary: Stats {
                total_transactions: self.total_trx,
                recoverable_transactions: self.recoverable_trx,
                skipped_events: self.skipped_events,
                estimated_rows_affected: self.rows_affected,
                recovery_rate,
            },
            warnings,
        }
    }

    /// Total events counted (total_transactions + skipped_events)
    pub fn total_events(&self) -> u64 {
        self.total_trx as u64 + self.skipped_events as u64
    }
}

/// Check if a query is a DDL statement
fn is_ddl_query(sql: &str) -> bool {
    let upper = sql.to_uppercase();
    upper.starts_with("CREATE")
        || upper.starts_with("ALTER")
        || upper.starts_with("DROP")
        || upper.starts_with("TRUNCATE")
        || upper.starts_with("RENAME")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::source::{RawEvent, RawKind};

    #[test]
    fn test_counter_new() {
        let counter = TransactionCounter::new();
        assert_eq!(counter.total_trx, 0);
        assert_eq!(counter.recoverable_trx, 0);
        assert_eq!(counter.skipped_events, 0);
        assert_eq!(counter.rows_affected, 0);
    }

    #[test]
    fn test_add_warning() {
        let mut counter = TransactionCounter::new();
        counter.add_warning("test_warning");
        counter.add_warning("test_warning");
        counter.add_warning("another_warning");

        assert_eq!(counter.warnings.get("test_warning"), Some(&2));
        assert_eq!(counter.warnings.get("another_warning"), Some(&1));
    }

    #[test]
    fn test_recovery_rate_calculation() {
        let test_cases = vec![
            (100, 100, "100.00%"),
            (100, 50, "50.00%"),
            (1000, 999, "99.90%"),
            (10, 3, "30.00%"),
            (0, 0, "0.00%"),
        ];

        for (total, recoverable, expected) in test_cases {
            let mut counter = TransactionCounter::new();
            counter.total_trx = total;
            counter.recoverable_trx = recoverable;

            let summary = counter.build_summary();
            assert_eq!(summary.summary.recovery_rate, expected);
        }
    }

    #[test]
    fn test_is_ddl_query() {
        assert!(is_ddl_query("CREATE TABLE t (id INT)"));
        assert!(is_ddl_query("ALTER TABLE t ADD COLUMN c INT"));
        assert!(is_ddl_query("DROP DATABASE db"));
        assert!(is_ddl_query("TRUNCATE TABLE t"));
        assert!(!is_ddl_query("SELECT * FROM t"));
        assert!(!is_ddl_query("INSERT INTO t VALUES (1)"));
        assert!(!is_ddl_query("BEGIN"));
    }
}
