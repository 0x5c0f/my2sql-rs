//! 保序器（spec §5.1）：dispatcher 顺序编号入队，worker 乱序完成回来后
//! 按 seq 连续弹出；空洞挂起等待补齐。
//!
//! 上游对照：my2sql-go 用自旋等待 `G_HandlingBinEventIndex`（events.go:176-193，
//! 忙等 + 全局锁）达成同款「按事件序输出」——本层按简报改用 HashMap 缓冲 +
//! 连续弹出（O(1) 摊销、无反射自旋），语义等价（输出序 = 事件序）。
//! 反压统一规则（简报 Step 2 / spec §5.1）：`pending() > 2×threads` 时
//! dispatcher 阻塞补收结果，直至回落。

use std::collections::HashMap;

use crate::pipeline::worker::SqlGroup;

/// 保序缓冲：`next` = 下一个待弹出的连续 seq；`buf` = 超前完成的乱序批次。
#[derive(Debug, Default)]
pub struct Reorder {
    next: u64,
    buf: HashMap<u64, Vec<SqlGroup>>,
}

impl Reorder {
    pub fn new() -> Self {
        Self::default()
    }

    /// 收入一个事件的完成批次：`seq` 恰为下一个 → 立即弹出并顺带清空后续连续段
    /// （返回可直接写出，通常非空）；否则挂缓冲等待空洞补齐（返回空）。
    /// seq 必须单调唯一（dispatcher 保证）；重复 push 覆盖缓冲（debug 下断言）。
    pub fn push(&mut self, seq: u64, g: Vec<SqlGroup>) -> Vec<SqlGroup> {
        debug_assert!(!self.buf.contains_key(&seq), "duplicate seq {seq} pushed");
        if seq != self.next {
            self.buf.insert(seq, g);
            return Vec::new();
        }
        let mut out = g;
        self.next += 1;
        while let Some(next) = self.buf.remove(&self.next) {
            out.extend(next);
            self.next += 1;
        }
        out
    }

    /// 缓冲中的乱序批次数（反压判据：`> 2×threads` 阻塞入队方）。
    pub fn pending(&self) -> usize {
        self.buf.len()
    }

    /// 收尾：全部 seq 到齐后缓冲应为空；若非空（上游断流/bug）按 seq 升序
    /// 强制吐出，不丢数据。
    pub fn drain_remaining(&mut self) -> Vec<SqlGroup> {
        let mut keys: Vec<u64> = self.buf.keys().copied().collect();
        keys.sort_unstable();
        let mut out = Vec::new();
        for k in keys {
            if let Some(g) = self.buf.remove(&k) {
                out.extend(g);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grp(db: &str, table: &str, sql: &str) -> Vec<SqlGroup> {
        vec![SqlGroup {
            binlog: "mysql-bin.000001".into(),
            start_pos: 4,
            end_pos: 100,
            timestamp: 10,
            db: db.into(),
            table: table.into(),
            trx_id: 1,
            sqls: vec![sql.into()],
        }]
    }

    #[test]
    fn in_order_push_emits_immediately() {
        let mut r = Reorder::new();
        assert_eq!(r.push(0, grp("d", "a", "s0")).len(), 1);
        assert_eq!(r.push(1, grp("d", "a", "s1")).len(), 1);
        assert_eq!(r.pending(), 0);
    }

    #[test]
    fn out_of_order_holds_gap_and_drains_contiguous_run() {
        let mut r = Reorder::new();
        assert!(
            r.push(2, grp("d", "a", "s2")).is_empty(),
            "seq1 缺席 → 挂起"
        );
        assert!(r.push(3, grp("d", "a", "s3")).is_empty());
        assert_eq!(r.pending(), 2);
        // 补 seq0：自身可弹出，但 seq1 仍是洞 → 连续段在 1 处停住
        let out = r.push(0, grp("d", "a", "s0"));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].sqls, vec!["s0"]);
        assert_eq!(r.pending(), 2);
        // 补 seq1：一次弹出 1..=3 全部连续段
        let out = r.push(1, grp("d", "a", "s1"));
        assert_eq!(
            out.iter().map(|g| g.sqls[0].as_str()).collect::<Vec<_>>(),
            vec!["s1", "s2", "s3"]
        );
        assert_eq!(r.pending(), 0);
    }

    #[test]
    fn gap_fill_flushes_all_contiguous_batches() {
        let mut r = Reorder::new();
        assert!(r.push(1, grp("d", "t", "b1")).is_empty());
        assert!(r.push(2, grp("d", "t", "b2")).is_empty());
        let out = r.push(0, grp("d", "t", "b0"));
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].sqls, vec!["b0"]);
        assert_eq!(out[2].sqls, vec!["b2"]);
        assert_eq!(r.pending(), 0);
    }

    #[test]
    fn empty_batches_participate_in_ordering() {
        // 错误跳过的 seq 推空批：不产出内容但必须填洞
        let mut r = Reorder::new();
        assert!(r.push(1, Vec::new()).is_empty());
        let out = r.push(0, grp("d", "t", "x"));
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn drain_remaining_returns_leftover_in_seq_order() {
        let mut r = Reorder::new();
        assert!(r.push(5, grp("d", "t", "s5")).is_empty());
        assert!(r.push(3, grp("d", "t", "s3")).is_empty());
        let rest = r.drain_remaining();
        assert_eq!(
            rest.iter().map(|g| g.sqls[0].as_str()).collect::<Vec<_>>(),
            vec!["s3", "s5"]
        );
        assert_eq!(r.pending(), 0);
    }
}
