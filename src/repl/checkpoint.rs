//! repl 模式断点档（P3 T3）：JSON 序列化的消费水位 + 已写文件清单，
//! **原子写**（隐藏 tmp + rename，POSIX 同目录 rename 对读者恒为整档新旧
//! 二态），以及启动自检用的 `written_files` ↔ 输出目录实物对账。
//! 契约（终审 FIX B 精确化）：**manifest 承诺而盘上缺失 = 硬错**
//! （产物被人删/档被篡改，续跑必基于假账）；**盘上多出未登记实物 =
//! `tracing::warn!` 放行**（at-least-once 崩溃恢复的预期形态——撕裂事务
//! 半块、rename 前崩溃的未登记产物，静默让运维看见即可，死锁恢复是误伤）。

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 断点档内容：binlog 文件 + 位点 + 人类可读时间戳 + 已写出的 sql 文件名
/// （相对输出目录、单一文件名片段）。serde 双端（磁盘 JSON ↔ 内存结构）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub file: String,
    pub pos: u32,
    pub ts: String,
    pub written_files: Vec<String>,
}

/// `read_verify` 错误面：IO / JSON 非法 / manifest 承诺的实物缺失
/// （Missing）/ 登记条目畸形（Malformed）。目录中多出的未登记实物**不是
/// 错误**（FIX B 契约：warn 放行，见模块头）。全变体消息均含定位文件名。
#[derive(Debug, thiserror::Error)]
pub enum CpError {
    #[error("checkpoint io: {0}")]
    Io(#[from] io::Error),
    #[error("invalid checkpoint json at {path:?}: {source}")]
    BadJson {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("checkpoint promises written file missing from disk: {0}")]
    Missing(String),
    #[error("malformed checkpoint written_files entry (not a single file name): {0}")]
    Malformed(String),
}

/// checkpoint 档的原子写 tmp 名：`.{file_name}.tmp`（隐藏档，与正式档同目录，
/// 保证 rename 原子性）。`write_atomic` 与 `read_verify` 共用本构造，两侧
/// 恒等——read_verify 据此豁免**本工具自己**崩溃留下的同名残骸。
fn own_tmp_name(path: &Path) -> Option<std::ffi::OsString> {
    let mut t = std::ffi::OsString::from(".");
    t.push(path.file_name()?);
    t.push(".tmp");
    Some(t)
}

/// `written_files` 条目卫生校验：登记的必须是**单一文件名片段**（本工具
/// `path_for` 产物的形态）——含 `/`、`\`、NUL、`..` 段或任何多段路径即畸形
/// （外来/篡改输入），对账前即硬错，绝不 `dir.join` 越界比对。
fn is_single_file_name(s: &str) -> bool {
    if s.is_empty() || s.contains(['/', '\\', '\0']) {
        return false;
    }
    let mut comps = std::path::Path::new(s).components();
    matches!(comps.next(), Some(std::path::Component::Normal(_))) && comps.next().is_none()
}

/// 原子写：`{dir}/.{file_name}.tmp` 落全量 JSON（write+flush+sync_all）后
/// rename 顶替正式档；任一步失败清掉 tmp（正式档保持旧内容，绝不被半截
/// 顶替）。既存残留 tmp 直接覆写，不影响正式档（rename 语义）。
///
/// ** durability 边界（诚实声明）**：`sync_all` 只保证 tmp **内容**落盘；
/// rename 本身不对目录项 fsync——所以本函数是**进程崩溃 durable**（读者
/// 恒见整档新/旧二态，POSIX 同目录 rename 原子），**不是掉电 durable**。
/// 极端掉电下 rename 可能丢失、回退到上一份 checkpoint，恢复时方向为
/// **安全重复消费**（重新拉取已写出的 binlog 区间），符合 repl 语义；
/// 刻意不加目录 fsync（性能与冻结面权衡，需要时另议）。
pub fn write_atomic(path: &Path, cp: &Checkpoint) -> io::Result<()> {
    let tmp_name = own_tmp_name(path)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "checkpoint 路径无文件名"))?;
    let tmp = path.with_file_name(tmp_name);
    let json = serde_json::to_vec(cp).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let done = (|| -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut f = File::create(&tmp)?;
        f.write_all(&json)?;
        f.flush()?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, path)?;
        Ok(())
    })();
    if done.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    done
}

/// 读档并交叉验证：JSON 合法后，先做 `written_files` 条目卫生校验（畸形
/// 名 → `Malformed` 硬错），再对账——manifest 承诺但盘上缺 → `Missing`
/// （硬错：档与实物脱节即拒猜，续跑必基于假账）；盘上多出未登记实物 →
/// `tracing::warn!` 放行（FIX B 契约：at-least-once 崩溃恢复的预期残骸，
/// 如撕裂事务半块；checkpoint 档自身与其 `.{name}.tmp` 崩溃残骸连告警都
/// 豁免——下次写原子档自然覆盖）。Missing/Malformed 消息均含文件名，
/// 未登记实物的告警同样含名，供运维定位清场范围。
pub fn read_verify(path: &Path, dir: &Path) -> Result<Checkpoint, CpError> {
    let raw = fs::read(path)?;
    let cp: Checkpoint = serde_json::from_slice(&raw).map_err(|source| CpError::BadJson {
        path: path.to_path_buf(),
        source,
    })?;
    for name in &cp.written_files {
        if !is_single_file_name(name) {
            return Err(CpError::Malformed(name.clone()));
        }
    }
    let listed: HashSet<&str> = cp.written_files.iter().map(String::as_str).collect();
    for name in &cp.written_files {
        if !dir.join(name).is_file() {
            return Err(CpError::Missing(name.clone()));
        }
    }
    let self_name = path.file_name();
    let own_tmp = own_tmp_name(path);
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if !entry.path().is_file() {
            continue; // 目录混装其他东西（如子目录）不在对账域
        }
        let fname = entry.file_name();
        if Some(fname.as_os_str()) == self_name {
            continue; // checkpoint 档自证不算多余实物
        }
        if Some(fname.as_os_str()) == own_tmp.as_deref() {
            continue; // 本工具自己的崩溃残留 tmp：瞬态垃圾，下次写会覆写
        }
        // FIX B 契约：未登记实物（含非 UTF-8 名——不可能是本工具写出的
        // 登记名）一律降级为告警放行；恢复死锁才是更大的罪。
        if !listed.contains(fname.to_string_lossy().as_ref()) {
            tracing::warn!(
                "repl resume reconcile: untracked file {} present in {} \
                 (at-least-once crash residue, proceeding; remove it manually \
                 if this is not expected)",
                fname.to_string_lossy(),
                dir.display()
            );
        }
    }
    Ok(cp)
}

#[cfg(test)]
mod tests {

    use std::fs;
    use std::path::PathBuf;
    use std::process;

    use super::{Checkpoint, CpError, read_verify, write_atomic};

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("my2sql-p3t3-cp-{}-{}", tag, process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn cp(files: &[&str]) -> Checkpoint {
        Checkpoint {
            file: "mysql-bin.000007".into(),
            pos: 4591,
            ts: "2026-09-21_12:00:00".into(),
            written_files: files.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// Step 1 测试①：写→读等值；残留 tmp 不顶正式档（rename 原子性）；
    /// read_verify 对账：缺实物（manifest 承诺而盘上无）→ 硬错且含文件名；
    /// 多实物 → FIX B 契约 warn 放行（见 `read_verify_warns_extras_...`）。
    #[test]
    fn checkpoint_roundtrip_and_rename_atomicity() {
        let dir = tmpdir("rt");
        fs::write(dir.join("to_sql.1.sql"), b"x").unwrap();
        fs::write(dir.join("to_sql.2.sql"), b"y").unwrap();
        let c = cp(&["to_sql.1.sql", "to_sql.2.sql"]);
        let path = dir.join("checkpoint.json");
        // 预置垃圾残留 tmp：write_atomic 必须照常工作，且 tmp 被 rename 消费
        let stray = dir.join(".checkpoint.json.tmp");
        fs::write(&stray, b"{not json").unwrap();
        write_atomic(&path, &c).unwrap();
        assert!(!stray.exists(), "残留 tmp 应已被 rename 消费，不顶正式档");
        assert_eq!(read_verify(&path, &dir).unwrap(), c, "写→读 roundtrip 等值");
        // checkpoint 档自身在 dir 内不得被误判为多余实物（上面已隐含验证）
        write_atomic(&path, &c).unwrap();
        assert_eq!(read_verify(&path, &dir).unwrap(), c, "重写幂等");

        // 缺实物：written_files 记了目录里没有的文件 → Missing 硬错，含文件名
        write_atomic(&path, &cp(&["to_sql.1.sql", "to_sql.9.sql"])).unwrap();
        let e = read_verify(&path, &dir).unwrap_err();
        assert!(
            matches!(&e, CpError::Missing(m) if m == "to_sql.9.sql"),
            "缺实物应 Missing(to_sql.9.sql)，got: {e}"
        );
        assert!(e.to_string().contains("to_sql.9.sql"), "错误消息含文件名");

        // 多实物：FIX B 契约——未登记实物告警放行，不再阻断（旧 Stale 硬错
        // 死锁崩溃恢复，终审裁定降级）。
        write_atomic(&path, &c).unwrap();
        fs::write(dir.join("to_sql.3.sql"), b"z").unwrap();
        assert_eq!(
            read_verify(&path, &dir).unwrap(),
            c,
            "多实物应 warn 放行（FIX B 契约）"
        );

        // JSON 非法 → 硬错（不是静默默认值）
        fs::write(&path, b"{oops").unwrap();
        let e = read_verify(&path, &dir).unwrap_err();
        assert!(matches!(e, CpError::BadJson { .. }), "非法 JSON 应 BadJson");

        fs::remove_dir_all(&dir).ok();
    }

    /// Fix round 1（Important）：本工具 `write_atomic` 在 create 与 rename
    /// 之间进程崩溃 → 目录里残留 `.{ckpt-name}.tmp`。下一次 `read_verify`
    /// 恰好在“崩溃后恢复”这一最需要成功的场景必须放行——自有 tmp 名属
    /// 瞬态垃圾，连告警都豁免（下次写会覆写）。FIX B 后其余未登记实物也
    /// 只 warn 不阻断（撕裂事务残骸是 at-least-once 的预期形态）。
    #[test]
    fn read_verify_tolerates_own_crash_leftover_tmp() {
        let dir = tmpdir("crashtmp");
        fs::write(dir.join("to_sql.1.sql"), b"x").unwrap();
        let c = cp(&["to_sql.1.sql"]);
        let path = dir.join("checkpoint.json");
        write_atomic(&path, &c).unwrap();
        // 模拟第二次 write_atomic 死在 File::create 与 rename 之间
        fs::write(dir.join(".checkpoint.json.tmp"), b"{half").unwrap();
        assert_eq!(
            read_verify(&path, &dir).unwrap(),
            c,
            "自有崩溃残留 tmp 不得误报阻断（崩溃恢复主场景）"
        );
        // 其余未登记实物：FIX B 契约——放行（旧 Stale 硬错死锁恢复，
        // 终审裁定降级为告警）；缺档面（Missing）仍 fail-loud，另件钉。
        fs::write(dir.join("to_sql.9.sql"), b"junk").unwrap();
        assert_eq!(
            read_verify(&path, &dir).unwrap(),
            c,
            "非 tmp 的游离实物按 FIX B 契约 warn 放行"
        );
        fs::remove_dir_all(&dir).ok();
    }

    /// 终审 FIX B 契约（正向钉）：manifest [a] + 盘上多出未登记实物
    /// （崩溃期撕裂事务半块等 at-least-once 预期残骸）→ **Ok 放行**；
    /// manifest 承诺 [a,b] 而 b 缺失 → 仍**硬错 Missing**。
    /// 修复前形态：多实物 → `Err(Stale)`，崩溃恢复恰在最需要续跑时死锁。
    #[test]
    fn read_verify_warns_extras_but_hard_errors_missing() {
        let dir = tmpdir("extras-warn");
        fs::write(dir.join("to_sql.1.sql"), b"x").unwrap();
        fs::write(dir.join("to_sql.9.sql"), b"partial-trashed-trx").unwrap();
        let path = dir.join("checkpoint.json");
        // 多实物：不再阻断（旧契约 Err Stale = 修复前红点）
        write_atomic(&path, &cp(&["to_sql.1.sql"])).unwrap();
        assert_eq!(
            read_verify(&path, &dir).unwrap(),
            cp(&["to_sql.1.sql"]),
            "未登记实物（崩溃残骸）应 warn 放行，不得硬错"
        );
        // 缺实物：manifest 承诺而盘上无 → 依旧硬错，含文件名
        write_atomic(&path, &cp(&["to_sql.1.sql", "to_sql.2.sql"])).unwrap();
        let e = read_verify(&path, &dir).unwrap_err();
        assert!(
            matches!(&e, CpError::Missing(m) if m == "to_sql.2.sql"),
            "缺实物必须仍 Missing(to_sql.2.sql)，got: {e}"
        );
        assert!(e.to_string().contains("to_sql.2.sql"), "错误消息含文件名");
        fs::remove_dir_all(&dir).ok();
    }

    /// Fix round 1（Minor）：`written_files` 登记的是**单一文件名片段**；
    /// 含路径分隔符或 `..` 的条目属畸形/外来输入，对账前即硬错（绝不
    /// `dir.join` 越界比对）。
    #[test]
    fn read_verify_rejects_malformed_written_files_entries() {
        let dir = tmpdir("malformed");
        let path = dir.join("checkpoint.json");
        for bad in ["../to_sql.1.sql", "sub/to_sql.1.sql", "..", "a\\b.sql"] {
            write_atomic(&path, &cp(&[bad])).unwrap();
            let e = read_verify(&path, &dir).unwrap_err();
            assert!(
                matches!(&e, CpError::Malformed(m) if m == bad),
                "条目 {bad} 应硬报 Malformed，got: {e}"
            );
            assert!(
                e.to_string().contains(bad),
                "错误消息须含畸形条目名 {bad}，got: {e}"
            );
        }
        fs::remove_dir_all(&dir).ok();
    }
}
