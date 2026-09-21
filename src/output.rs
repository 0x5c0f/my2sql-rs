//! 输出层：按 binlog 序号/表切分 `.sql` 文件（或 stdout），负责文件头与
//! extra-info 注释的**字节面**（T13 遗留至此的 `--add-extra-info` 包表层）。
//!
//! ## 上游字节对照（my2sql-go base/events.go，权威引用）
//!
//! - extra-info 注释行（`GetForwardRollbackContentLineWithExtra`，
//!   events.go:322-326）：
//!   `# datetime=%s database=%s table=%s binlog=%s startpos=%d stoppos=%d\n`
//!   ——字段名/顺序/单空格逐字节镜像（简报速记 `startpos/stoppos` 与上游一致，
//!   本次实读复核第 8 例无失真）。
//! - **datetime 渲染格式**：上游取值 `GetDatetimeStr(ev.Timestamp, 0,
//!   constvar.DATETIME_FORMAT_NOSPACE)`（events.go:170 +
//!   constvar.go:6 `"2006-01-02_15:04:05"`）——**日期与时间之间是下划线**，
//!   且 `time.Unix(..).Format(..)` 用**运行进程的主机时区**。本工具：
//!   `--time-zone` 偏移（T1 对）施加到 unix 秒后手工格式化，保持下划线形态
//!   （确定性输出、不依赖运行机器 TZ；T15 差异：裁判跑固定 TZ 即可复现）。
//!   chrono 仅在本输出边沿使用（CLI 专用约束下允许的边界，见 HANDOVER T14 注）。
//! - 语句行：上游 `strings.Join(sqls, ";\n") + ";\n"`（本层 `DmlBuilder`
//!   产句自带 `;`，逐句 + `\n` 写出，字节等价）。
//! - 文件名：上游 `forward.{schema.table.}{N}.sql`（events.go:286-299，
//!   N=`%d` 十进制序号）；本工具按 spec 采 Python my2sql 命名族
//!   `to_sql.{schema.table.}<N>.sql`（N 去前导零、无补宽）。
//! - `SET NAMES utf8mb4;` 文件头 = 本项目计划约束（上游 Go 版无此行，
//!   python my2sql 有；T15 白名单已挂「SET NAMES 头」项）。

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use chrono::{FixedOffset, TimeZone};

use crate::pipeline::worker::SqlGroup;

/// 新输出文件创建时写入的头部（计划约束，逐字节钉死于 e2e）。
pub const FILE_HEADER: &str = "SET NAMES utf8mb4;\n";

/// event unix 秒 → `datetime` 分量文本（上游 `2006-01-02_15:04:05` 下划线形，
/// 见模块注释；u32 秒恒可解析）。
pub fn datetime_str(ts: u32, tz: FixedOffset) -> String {
    tz.timestamp_opt(ts as i64, 0)
        .single()
        .expect("u32 seconds always valid in fixed offset")
        .format("%Y-%m-%d_%H:%M:%S")
        .to_string()
}

/// **仅供路径使用**的名字净化（T14 审阅 Finding 2，敌意 TABLE_MAP 防线）：
/// `/`、`\`、NUL → `?`；整段恰为 `..` → `?`。替换后 db/table 成为单一普通
/// 路径段，`dir.join` 恒得 `parent()==dir`。**SqlGroup.db/table 原字节不动**
/// ——extra-info 注释与反引号 SQL 文本仍消费原始值（净化只发生在拼 PathBuf
/// 这一处）。与上游 my2sql 的有意偏差：上游同款插值可越界写（挂账 T15）。
fn sanitize_for_path(s: &str) -> std::borrow::Cow<'_, str> {
    if s != ".." && !s.contains(['/', '\\', '\0']) {
        return std::borrow::Cow::Borrowed(s);
    }
    if s == ".." {
        return std::borrow::Cow::Borrowed("?");
    }
    std::borrow::Cow::Owned(
        s.chars()
            .map(|c| match c {
                '/' | '\\' | '\0' => '?',
                _ => c,
            })
            .collect(),
    )
}

/// 输出目标路径：`{prefix}.{schema.table.}<N>.sql`（file_per_table 前半段取舍），
/// N = binlog 末段 `.` 后十进制序号（去前导零）；无数字后缀 → 0（与
/// `filter::split_binlog_name` 同款回退，正常文件名恒带序号）。
/// db/table 先经 `sanitize_for_path`（仅路径面，见其上注释）。
/// P2 T2：前缀参数化——to-sql 传 `"to_sql"`，flashback tmp 传
/// `".flashback.tmp"`（隐藏文件），final 传 `"flashback"`。
pub fn path_for(
    dir: &Path,
    prefix: &str,
    binlog: &str,
    db: &str,
    table: &str,
    file_per_table: bool,
) -> PathBuf {
    let n = binlog_index(binlog);
    if file_per_table {
        dir.join(format!(
            "{prefix}.{}.{}.{}.sql",
            sanitize_for_path(db),
            sanitize_for_path(table),
            n
        ))
    } else {
        dir.join(format!("{prefix}.{n}.sql"))
    }
}

fn binlog_index(binlog: &str) -> u64 {
    match binlog.rfind('.') {
        Some(i) => {
            let suffix = &binlog[i + 1..];
            if !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()) {
                suffix.parse::<u64>().unwrap_or(0)
            } else {
                0
            }
        }
        None => 0,
    }
}

/// 写出器：文件句柄缓存（按路径）+ 首建写文件头；`stdout=true` 时全部
/// 内容并入标准输出（同字节流形态，含 SET NAMES 头与 extra-info——与文件
/// 模式统一，区别于上游屏幕模式仅打语句，偏差记录 HANDOVER T14）。
pub struct Writer {
    dir: PathBuf,
    stdout: bool,
    file_per_table: bool,
    extra_info: bool,
    tz: FixedOffset,
    /// 文件名前缀段（`path_for` 首段）：to-sql = `to_sql`，
    /// flashback tmp = `.flashback.tmp`（P2 T2 前缀参数化）。
    prefix: String,
    /// true = 块索引模式：每写一个 rows-event 批次登记
    /// `(offset, len, trx_id)`，供 flashback 逆序回读（偏移仅文件 sink）。
    index: bool,
    sinks: HashMap<PathBuf, Sink>,
    /// 块索引：tmp 路径 → `Vec<(offset, len, trx_id)>`（写入顺序）。
    blocks: HashMap<PathBuf, Vec<(u64, u64, u64)>>,
    /// 创建顺序（finish 返回文件数、测试稳定性）。
    created: Vec<PathBuf>,
}

enum Sink {
    File {
        bw: BufWriter<File>,
        /// 该文件已落字节数（含 FILE_HEADER），= 下一批次写入的偏移。
        written: u64,
    },
    Screen,
}

impl Writer {
    pub fn new(
        dir: PathBuf,
        stdout: bool,
        file_per_table: bool,
        extra_info: bool,
        tz: FixedOffset,
        prefix: String,
        index: bool,
    ) -> Self {
        Self {
            dir,
            stdout,
            file_per_table,
            extra_info,
            tz,
            prefix,
            index,
            sinks: HashMap::new(),
            blocks: HashMap::new(),
            created: Vec::new(),
        }
    }

    /// 块索引只读视图（flashback 管线消费；index=false 时恒为空表）。
    pub fn blocks(&self) -> &HashMap<PathBuf, Vec<(u64, u64, u64)>> {
        &self.blocks
    }

    fn sink_key(&self, g: &SqlGroup) -> PathBuf {
        if self.stdout {
            PathBuf::from("<stdout>")
        } else {
            path_for(
                &self.dir,
                &self.prefix,
                &g.binlog,
                &g.db,
                &g.table,
                self.file_per_table,
            )
        }
    }

    fn ensure_sink(&mut self, key: &Path) -> std::io::Result<()> {
        if !self.sinks.contains_key(key) {
            let sink = if self.stdout {
                Sink::Screen
            } else {
                if let Some(parent) = key.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let f = File::create(key)?;
                let mut bw = BufWriter::new(f);
                bw.write_all(FILE_HEADER.as_bytes())?;
                self.created.push(key.to_path_buf());
                Sink::File {
                    bw,
                    written: FILE_HEADER.len() as u64,
                }
            };
            self.sinks.insert(key.to_path_buf(), sink);
        }
        Ok(())
    }

    /// 写一个批次（extra-info 时前置注释行；语句自带 `;`，逐句一行）。
    /// 空批次不写任何东西（含注释行——不留悬空元数据）。
    pub fn write_group(&mut self, g: &SqlGroup) -> std::io::Result<()> {
        if g.sqls.is_empty() {
            return Ok(());
        }
        // 先组装本批完整字节串（注释行+逐句+`\n`），索引与写入共用同一 len。
        let mut batch = String::new();
        if self.extra_info {
            // 读取 self 字段后再取 sink（可变借用 self），避免借用冲突。
            batch.push_str(&format!(
                "# datetime={} database={} table={} binlog={} startpos={} stoppos={}\n",
                datetime_str(g.timestamp, self.tz),
                g.db,
                g.table,
                g.binlog,
                g.start_pos,
                g.end_pos
            ));
        }
        for sql in &g.sqls {
            batch.push_str(sql);
            batch.push('\n');
        }
        let key = self.sink_key(g);
        self.ensure_sink(&key)?;
        let sink = self.sinks.get_mut(&key).expect("just inserted/left in map");
        if self.index {
            // stdout（Screen sink）无字节偏移概念：仅文件 sink 登记。
            if let Sink::File { written, .. } = sink {
                self.blocks.entry(key.clone()).or_default().push((
                    *written,
                    batch.len() as u64,
                    g.trx_id,
                ));
            }
        }
        sink.write_all(batch.as_bytes())?;
        if let Sink::File { written, .. } = sink {
            *written += batch.len() as u64;
        }
        sink.flush_short()
    }

    /// 收尾：flush 全部句柄，返回写出的文件数（stdout 模式 = 0）。
    pub fn finish(&mut self) -> std::io::Result<usize> {
        for sink in self.sinks.values_mut() {
            sink.flush()?;
        }
        Ok(self.created.len())
    }
}

impl Sink {
    fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        match self {
            Sink::File { bw, .. } => bw.write_all(buf),
            Sink::Screen => std::io::stdout().write_all(buf),
        }
    }
    /// Screen 每批次即刷（交互体验）；文件靠 BufWriter 批量 + finish 统一刷。
    fn flush_short(&mut self) -> std::io::Result<()> {
        match self {
            Sink::File { .. } => Ok(()),
            Sink::Screen => self.flush(),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Sink::File { bw, .. } => bw.flush(),
            Sink::Screen => std::io::stdout().flush(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grp(binlog: &str, db: &str, table: &str) -> SqlGroup {
        SqlGroup {
            binlog: binlog.into(),
            start_pos: 1020,
            end_pos: 3358,
            timestamp: 1755999999,
            db: db.into(),
            table: table.into(),
            trx_id: 1,
            sqls: vec!["SELECT 1;".into()],
        }
    }

    #[test]
    fn path_for_naming_matrix() {
        let d = Path::new("/out");
        // file_per_table：`{prefix}.{schema.table.}<N>.sql`；N 去前导零（%d 口径）
        assert_eq!(
            path_for(d, "to_sql", "mysql-bin.000003", "t10", "u", true),
            PathBuf::from("/out/to_sql.t10.u.3.sql")
        );
        assert_eq!(
            path_for(d, "to_sql", "mysql-bin.000003", "t10", "u", false),
            PathBuf::from("/out/to_sql.3.sql")
        );
        // 6 位以上不截断（与 next_binlog_name 的 %06d 最小宽口径呼应）
        assert_eq!(
            path_for(d, "to_sql", "mysql-bin.1000000", "a", "b", false),
            PathBuf::from("/out/to_sql.1000000.sql")
        );
        // 无数字后缀 → 0（回退，注释已录）
        assert_eq!(
            path_for(d, "to_sql", "weird", "a", "b", true),
            PathBuf::from("/out/to_sql.a.b.0.sql")
        );
    }

    /// Finding 2（RED→GREEN）：敌意 TABLE_MAP db/table 的路径穿越。净化**仅
    /// 作用于构造 PathBuf**；SqlGroup 内 db/table 原字节（extra-info/SQL 文本
    /// 消费面）不受影响。攻击形态：明文 `a/../../../evil/x`、`..`、反斜杠、NUL。
    #[test]
    fn path_for_sanitizes_traversal_names_into_dir() {
        let d = Path::new("/out");
        // 明文多段穿越：结果必须仍是 dir 的直接子文件（parent == dir）
        let p = path_for(
            d,
            "to_sql",
            "mysql-bin.000003",
            "a/../../../evil",
            "x",
            true,
        );
        assert_eq!(p.parent(), Some(d), "穿越名不得逃出目录: {p:?}");
        // `..` 独立段
        let p = path_for(d, "to_sql", "mysql-bin.000003", "..", "x", true);
        assert_eq!(p, PathBuf::from("/out/to_sql.?.x.3.sql"));
        // 反斜杠（Windows 形）与 NUL
        let p = path_for(d, "to_sql", "mysql-bin.000003", "a\\b", "c\0d", true);
        assert_eq!(p, PathBuf::from("/out/to_sql.a?b.c?d.3.sql"));
        // 净化后的 file_name 不含任何路径分隔符/裸 `..` 段
        let p = path_for(
            d,
            "to_sql",
            "mysql-bin.000003",
            "a/../../../evil",
            "x",
            true,
        );
        let f = p.file_name().unwrap().to_str().unwrap().to_string();
        assert!(!f.contains('/') && !f.contains('\\') && !f.contains('\0'));
        // 干净名零改动（净化对正常路径为恒等映射）
        assert_eq!(
            path_for(d, "to_sql", "mysql-bin.000003", "t10", "u", true),
            PathBuf::from("/out/to_sql.t10.u.3.sql")
        );
    }

    #[test]
    fn datetime_uses_underscore_form_and_fixed_offset() {
        // 上游 constvar.DATETIME_FORMAT_NOSPACE = "2006-01-02_15:04:05"
        let utc = FixedOffset::east_opt(0).unwrap();
        assert_eq!(datetime_str(0, utc), "1970-01-01_00:00:00");
        assert_eq!(datetime_str(3600, utc), "1970-01-01_01:00:00");
        // +08:00 偏移生效（zone 由 --time-zone 决定，非本机 TZ）
        let plus8 = FixedOffset::east_opt(8 * 3600).unwrap();
        assert_eq!(datetime_str(3600, plus8), "1970-01-01_09:00:00");
    }

    #[test]
    fn extra_info_line_matches_upstream_format() {
        // 逐字节对照 events.go:323 模板（datetime 下划线形、单空格、字段序）
        let utc = FixedOffset::east_opt(0).unwrap();
        let line = format!(
            "# datetime={} database={} table={} binlog={} startpos={} stoppos={}\n",
            datetime_str(1755999999, utc),
            "t10",
            "u",
            "mysql-bin.000003",
            1020u32,
            3358u32
        );
        assert_eq!(
            line,
            "# datetime=2025-08-24_01:46:39 database=t10 table=u binlog=mysql-bin.000003 startpos=1020 stoppos=3358\n"
        );
    }

    #[test]
    fn writer_emits_header_once_and_appends_groups() {
        let dir = std::env::temp_dir().join(format!("my2sql-t14-w-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        {
            let mut w = Writer::new(
                dir.clone(),
                false,
                false,
                true,
                FixedOffset::east_opt(0).unwrap(),
                "to_sql".into(),
                false,
            );
            let mut g = grp("mysql-bin.000001", "d", "t");
            w.write_group(&g).unwrap();
            g.sqls.push("SELECT 2;".into());
            w.write_group(&g).unwrap();
            assert_eq!(w.finish().unwrap(), 1);
        }
        let text = std::fs::read_to_string(dir.join("to_sql.1.sql")).unwrap();
        assert_eq!(
            text,
            "SET NAMES utf8mb4;\n\
             # datetime=2025-08-24_01:46:39 database=d table=t binlog=mysql-bin.000001 startpos=1020 stoppos=3358\n\
             SELECT 1;\n\
             # datetime=2025-08-24_01:46:39 database=d table=t binlog=mysql-bin.000001 startpos=1020 stoppos=3358\n\
             SELECT 1;\nSELECT 2;\n"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Finding 2（RED→GREEN）写盘面：穿越名经 Writer 后不得在 dir 之外创建
    /// 任何文件/目录（旧行为：sink_for 的 create_dir_all 会亲手铺出逃逸目录），
    /// 且 extra-info 注释保留 db/table **原始字节**（净化仅路径用）。
    #[test]
    fn writer_traversal_names_never_escape_output_dir() {
        fn count_files(p: &Path) -> usize {
            if p.is_file() {
                1
            } else if p.is_dir() {
                std::fs::read_dir(p)
                    .map(|rd| {
                        rd.filter_map(|e| e.ok())
                            .map(|e| count_files(&e.path()))
                            .sum()
                    })
                    .unwrap_or(0)
            } else {
                0
            }
        }
        let root = std::env::temp_dir().join(format!("my2sql-t14-esc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let dir = root.join("out");
        {
            let mut w = Writer::new(
                dir.clone(),
                false,
                true,
                true,
                FixedOffset::east_opt(0).unwrap(),
                "to_sql".into(),
                false,
            );
            w.write_group(&grp("mysql-bin.000001", "a/../../evil", "x"))
                .unwrap();
            w.write_group(&grp("mysql-bin.000001", "..", "..")).unwrap();
            assert_eq!(w.finish().unwrap(), 2);
        }
        // 全部落盘恰好 2 个文件且都在 dir 内（root 树下无逃逸目录）
        assert_eq!(count_files(&root), 2, "no file escaped dir");
        let names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 2);
        assert!(names.iter().all(|n| !n.contains('/') && !n.contains('\\')));
        // 净化仅供路径：extra-info 行仍带原始穿越字节
        let escaped = names
            .iter()
            .find(|n| n.starts_with("to_sql.a"))
            .expect("sanitized traversal file exists");
        let text = std::fs::read_to_string(dir.join(escaped)).unwrap();
        assert!(
            text.contains("database=a/../../evil table=x"),
            "extra-info 保留原字节, got: {text}"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// P2 T2 Step 1：flashback tmp 块索引模式——每 rows-event 批一次
    /// `(offset, len, trx_id)` 登记（非按事务合并），偏移按字节精确
    /// （首块紧跟 SET NAMES 头、末块止于文件尾），块内语句保序。
    #[test]
    fn writer_indexes_blocks_for_flashback_tmp() {
        let dir = std::env::temp_dir().join(format!("my2sql-p2t2-b-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let tmp = dir.join(".flashback.tmp.1.sql");
        {
            let mut w = Writer::new(
                dir.clone(),
                false,
                false,
                true,
                FixedOffset::east_opt(0).unwrap(),
                ".flashback.tmp".into(),
                true,
            );
            let mut g = grp("mysql-bin.000001", "d", "t"); // trx_id=1, sqls=["SELECT 1;"]
            w.write_group(&g).unwrap(); // 注释行 + 1 SQL
            g.trx_id = 2;
            g.sqls.push("SELECT 2;".into());
            w.write_group(&g).unwrap();
            assert_eq!(w.finish().unwrap(), 1);
            let idx = w.blocks().get(&tmp).expect("block index for tmp");
            let total = std::fs::metadata(&tmp).unwrap().len();
            assert_eq!(idx.len(), 2);
            assert_eq!(idx[0].0, FILE_HEADER.len() as u64, "首块紧跟 SET NAMES 头");
            assert_eq!(idx[1].0 + idx[1].1, total, "末块止于文件尾");
            assert_eq!((idx[0].2, idx[1].2), (1, 2), "trx_id 逐块透传");
            let bytes = std::fs::read(&tmp).unwrap();
            let b0 = &bytes[idx[0].0 as usize..][..idx[0].1 as usize];
            let b1 = &bytes[idx[1].0 as usize..][..idx[1].1 as usize];
            assert!(
                b0.starts_with(b"# datetime=") && b0.ends_with(b"SELECT 1;\n"),
                "{b0:?}"
            );
            assert!(
                b1.starts_with(b"# datetime=") && b1.ends_with(b"SELECT 2;\n"),
                "{b1:?}"
            );
            assert!(
                String::from_utf8_lossy(b1).contains("SELECT 1;\nSELECT 2;\n"),
                "块内语句保序（逆序属 reverse.rs）"
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn writer_file_per_table_splits_and_names_correctly() {
        let dir = std::env::temp_dir().join(format!("my2sql-t14-wpt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        {
            let mut w = Writer::new(
                dir.clone(),
                false,
                true,
                false,
                FixedOffset::east_opt(0).unwrap(),
                "to_sql".into(),
                false,
            );
            w.write_group(&grp("mysql-bin.000002", "d", "a")).unwrap();
            w.write_group(&grp("mysql-bin.000002", "d", "b")).unwrap();
            assert_eq!(w.finish().unwrap(), 2);
        }
        assert!(dir.join("to_sql.d.a.2.sql").is_file());
        assert!(dir.join("to_sql.d.b.2.sql").is_file());
        let a = std::fs::read_to_string(dir.join("to_sql.d.a.2.sql")).unwrap();
        assert_eq!(
            a, "SET NAMES utf8mb4;\nSELECT 1;\n",
            "无 extra-info 仅头+语句"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
