//! P2 flashback：正序 tmp → 逆序回滚脚本（顺序反转层）。
//! 语义反转在 sqlopen::dml（WorkKind），本模块只管文件舞蹈。
//! 命名族（有异于上游 rollback.*，CLI 重设计既定）：
//! final `flashback.{N}.sql` / file-per-table `flashback.{db}.{table}.{N}.sql`;
//! tmp 为隐藏文件 `.flashback.tmp.{N}[.db.table]` 收尾删除。

pub mod report;
pub mod reverse;

use std::path::{Path, PathBuf};

/// tmp 路径 → final 路径：把文件名的 `.flashback.tmp` 前导段替换为
/// `flashback`（`.flashback.tmp.3.sql` → `flashback.3.sql`；
/// file-per-table 形态 `.flashback.tmp.db.tb.3.sql` → `flashback.db.tb.3.sql`，
/// path_for 前缀恒为首段，见 output.rs 6 参版；db/table 已经
/// `sanitize_for_path` 净化，替换对任意中间段透明）。
/// 无 `file_name()`（如 `..`/根）→ 原样返回。
pub fn final_for_tmp(tmp: &Path) -> PathBuf {
    tmp.file_name()
        .map(|f| {
            let s = f.to_string_lossy();
            PathBuf::from(s.replacen(".flashback.tmp", "flashback", 1))
        })
        .unwrap_or_else(|| tmp.to_path_buf())
}
