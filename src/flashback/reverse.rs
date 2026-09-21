//! 顺序反转（spec §3.1）：正序 tmp + (offset,len,trx_id) 块索引 → 并行从尾
//! 回读。字节口径对照上游 rollback_process.go（模块级差异=记录原子化+前缀注入
//! 照抄，见 plan T2 测试注释）。

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::output::FILE_HEADER;

/// 块索引条目：`(offset, len, trx_id)`（tmp 文件内一块 = 一个 rows-event 批）。
pub type Block = (u64, u64, u64);

/// 块内逆序：extra-info 注释行与其 SQL 组为原子记录（注释保头），SQL 行逆序。
pub fn reverse_block(text: &str) -> String {
    let mut lines: Vec<&str> = text.split_terminator('\n').collect();
    let comment = if lines.first().is_some_and(|l| l.starts_with("# datetime=")) {
        Some(lines.remove(0))
    } else {
        None
    };
    lines.reverse();
    let mut out = String::with_capacity(text.len());
    if let Some(c) = comment {
        out.push_str(c);
        out.push('\n');
    }
    for l in lines {
        out.push_str(l);
        out.push('\n');
    }
    out
}

/// 单 tmp → final（调用方保证 blocks 非空）。keep_trx=true 时逐字节复刻上游
/// 注入：首块前注入（last=0 而 trx≥1）、trx 变化处注入、尾 `commit;\n`。
pub fn reverse_file(
    tmp: &Path,
    out: &Path,
    blocks: &[Block],
    keep_trx: bool,
    warn_line: Option<&str>,
) -> std::io::Result<()> {
    let mut src = File::open(tmp)?;
    let mut dst = BufWriter::new(
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(out)?,
    );
    dst.write_all(FILE_HEADER.as_bytes())?;
    if let Some(w) = warn_line {
        dst.write_all(w.as_bytes())?;
    }
    let mut last_trx: u64 = 0;
    for (off, len, trx) in blocks.iter().rev() {
        let mut buf = vec![0u8; *len as usize];
        src.seek(SeekFrom::Start(*off))?;
        src.read_exact(&mut buf)?;
        if keep_trx && last_trx != *trx {
            dst.write_all(b"commit;\nbegin;\n")?;
        }
        last_trx = *trx;
        dst.write_all(reverse_block(&String::from_utf8_lossy(&buf)).as_bytes())?;
    }
    if keep_trx {
        dst.write_all(b"commit;\n")?;
    }
    dst.flush()
}

/// 并行驱动：文件级任务队列（单文件内不再切分——上游同口径
/// events.go:272-282 每文件一线程），threads 只影响文件间并发。
/// 成功即删 tmp；任一文件失败 → 汇总返回 Err（调用方清场）。
pub fn run_files(
    files: &[(PathBuf, PathBuf, Vec<Block>)],
    keep_trx: bool,
    threads: usize,
    warn_line: Option<&str>,
) -> std::io::Result<()> {
    let queue = Arc::new(Mutex::new(files.iter().cloned().collect::<VecDeque<_>>()));
    let err = Arc::new(Mutex::new(None::<std::io::Error>));
    let mut hs = Vec::new();
    for _ in 0..threads.clamp(1, files.len().max(1)) {
        let (q, e, k, w) = (
            queue.clone(),
            err.clone(),
            keep_trx,
            warn_line.map(str::to_string),
        );
        hs.push(std::thread::spawn(move || {
            loop {
                let task = q.lock().unwrap().pop_front();
                let Some((tmp, out, blocks)) = task else {
                    break;
                };
                if blocks.is_empty() {
                    let _ = std::fs::remove_file(&tmp);
                    continue;
                }
                if let Err(ioe) = reverse_file(&tmp, &out, &blocks, k, w.as_deref()) {
                    *e.lock().unwrap() = Some(ioe);
                    return;
                }
                std::fs::remove_file(&tmp).ok();
            }
        }));
    }
    let mut join_err = None;
    for h in hs {
        if h.join().is_err() && join_err.is_none() {
            join_err = Some(std::io::Error::other("reverse worker panicked"));
        }
    }
    let stored = err.lock().unwrap().take();
    match (stored, join_err) {
        (Some(e), _) => Err(e),
        (None, Some(e)) => Err(e),
        (None, None) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_tmp(path: &Path, blocks: &[&str]) -> Vec<(u64, u64, u64)> {
        let mut f = std::fs::File::create(path).unwrap();
        std::io::Write::write_all(&mut f, crate::output::FILE_HEADER.as_bytes()).unwrap();
        let mut off = crate::output::FILE_HEADER.len() as u64;
        let mut idx = Vec::new();
        for (i, b) in blocks.iter().enumerate() {
            f.write_all(b.as_bytes()).unwrap();
            idx.push((off, b.len() as u64, (i as u64 / 2) + 1)); // 每两块一事务
            off += b.len() as u64;
        }
        idx
    }

    #[test]
    fn reverse_bytes_byte_equal_upstream_keeptrx_quirk() {
        // 上游口径（rollback_process.go:31,131-133,153-155）：lastTrxIdx 初值 0，
        // 首个写出块（tmp 尾块）必注入 commit;\nbegin;\n（头部悬空 commit 是其原样
        // 行为，「逐字节对齐」= 照抄）；块间 trx 变化处注入；尾补 commit;\n。
        let dir = std::env::temp_dir().join(format!("my2sql-p2t2-r-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tmp = dir.join(".flashback.tmp.1.sql");
        let outp = dir.join("flashback.1.sql");
        // 块 = 单行 SQL（无 extra-info 形态 = 与上游裸行逆序全等）
        let idx = write_tmp(
            &tmp,
            &["INSERT A;\n", "INSERT B;\n", "INSERT C;\n", "INSERT D;\n"],
        );
        run_files(&[(tmp.clone(), outp.clone(), idx)], true, 2, None).unwrap();
        assert_eq!(
            std::fs::read_to_string(&outp).unwrap(),
            "SET NAMES utf8mb4;\n\
             commit;\nbegin;\nINSERT D;\nINSERT C;\n\
             commit;\nbegin;\nINSERT B;\nINSERT A;\n\
             commit;\n"
        );
        assert!(!tmp.exists(), "tmp 必须删除（上游 rollback_process.go:20）");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn reverse_block_is_record_atomic_with_comment_first() {
        // 超越项 1：注释行不漂到组尾；块内 SQL 行逆序（语义所需）
        let blk = "# datetime=X stoppos=99\nINSERT r1;\nINSERT r2;\n";
        assert_eq!(
            reverse_block(blk),
            "# datetime=X stoppos=99\nINSERT r2;\nINSERT r1;\n"
        );
        // 无注释块 = 纯行逆序（与上游一致）
        assert_eq!(reverse_block("A;\nB;\n"), "B;\nA;\n");
    }

    #[test]
    fn no_keeptrx_emits_pure_reverse_without_scaffold() {
        let dir = std::env::temp_dir().join(format!("my2sql-p2t2-nk-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tmp = dir.join(".flashback.tmp.1.sql");
        let outp = dir.join("flashback.1.sql");
        // 两块同事务（write_tmp 的 i/2+1 规则：块0/1=trx1、块2/3=trx2）→
        // keep_trx=false：仅逆序、零脚手架。块含多行：块内行逆序照旧。
        let idx = write_tmp(&tmp, &["A1;\nA2;\n", "B1;\n", "C1;\n", "D1;\n"]);
        run_files(&[(tmp.clone(), outp.clone(), idx)], false, 1, None).unwrap();
        assert_eq!(
            std::fs::read_to_string(&outp).unwrap(),
            "SET NAMES utf8mb4;\nD1;\nC1;\nB1;\nA2;\nA1;\n"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn threads_1_and_8_output_identical_across_files() {
        // 文件级任务队列：threads 只改文件间并发 → 3 文件两跑（threads 1/8）
        // 逐文件字节全等（含 keep-trx 注入的 per-file lastTrxIdx 复位语义：
        // 每文件独立 0 初值，与上游 per-file 线程一致）。
        let dir = std::env::temp_dir().join(format!("my2sql-p2t2-th-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut jobs1 = Vec::new();
        for n in 1..=3 {
            let tmp = dir.join(format!(".flashback.tmp.{n}.sql"));
            let outp = dir.join(format!("flashback.{n}.sql"));
            let idx = write_tmp(&tmp, &["s1;\n", "s2;\n", "s3;\n"]);
            jobs1.push((tmp, outp, idx));
        }
        run_files(&jobs1, true, 1, None).unwrap();
        let first: Vec<String> = jobs1
            .iter()
            .map(|j| std::fs::read_to_string(&j.1).unwrap())
            .collect();
        for j in &mut jobs1 {
            std::fs::remove_file(&j.1).unwrap();
            let idx = write_tmp(&j.0, &["s1;\n", "s2;\n", "s3;\n"]); // 重跑需重建 tmp
            j.2 = idx;
        }
        run_files(&jobs1, true, 8, None).unwrap();
        let second: Vec<String> = jobs1
            .iter()
            .map(|j| std::fs::read_to_string(&j.1).unwrap())
            .collect();
        assert_eq!(first, second, "threads 不得影响单文件字节");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn warn_line_inserted_after_header_and_empty_index_skipped() {
        let dir = std::env::temp_dir().join(format!("my2sql-p2t2-wl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tmp = dir.join(".flashback.tmp.1.sql");
        let outp = dir.join("flashback.1.sql");
        let idx = write_tmp(&tmp, &["X;\n"]);
        run_files(
            &[(tmp.clone(), outp.clone(), idx)],
            false,
            1,
            Some("-- WARNING: skipped 3 events, positions in stderr\n"),
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(&outp).unwrap(),
            "SET NAMES utf8mb4;\n-- WARNING: skipped 3 events, positions in stderr\nX;\n"
        );
        // 空块表：不落 final、tmp 删除、Ok（与上游产出仅 commit;\n 空文件的分歧
        // 登记入差异清单——判空跳过）
        let tmp2 = dir.join(".flashback.tmp.2.sql");
        std::fs::File::create(&tmp2)
            .unwrap()
            .write_all(crate::output::FILE_HEADER.as_bytes())
            .unwrap();
        let outp2 = dir.join("flashback.2.sql");
        run_files(&[(tmp2.clone(), outp2.clone(), Vec::new())], true, 1, None).unwrap();
        assert!(!outp2.exists() && !tmp2.exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn final_for_tmp_plain_form() {
        // 口径（简报钉死）：只换 file_name 段（父目录由调用方 join），
        // tmp 隐藏前缀段 → final 公开前缀段，序号不动
        assert_eq!(
            crate::flashback::final_for_tmp(std::path::Path::new("/out/.flashback.tmp.3.sql")),
            PathBuf::from("flashback.3.sql")
        );
    }

    #[test]
    fn final_for_tmp_file_per_table_form() {
        // file-per-table（path_for 6 参版：`.flashback.tmp.{db}.{tb}.{N}.sql`）；
        // 假设：db/table 已过 sanitize_for_path（穿越名净化为 `?`，此处仅钉
        // 前缀替换对任意中间段透明）。
        assert_eq!(
            crate::flashback::final_for_tmp(std::path::Path::new("/out/.flashback.tmp.d.t.3.sql")),
            PathBuf::from("flashback.d.t.3.sql")
        );
        assert_eq!(
            crate::flashback::final_for_tmp(std::path::Path::new(
                "/out/.flashback.tmp.a?.b?.7.sql"
            )),
            PathBuf::from("flashback.a?.b?.7.sql")
        );
    }
}
