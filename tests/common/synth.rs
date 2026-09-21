//! 合成 binlog 构造器（P2 T3 从 `tests/e2e.rs` 整段平移，行为零变；
//! e2e.rs 经 `#[path = "common/synth.rs"] mod synth;` 引用）。
//!
//! 合成 binlog 无 CRC（5.6 语义，镜像 `src/binlog/file_reader.rs` 测试 Synth），
//! 事件体布局严格对齐 `decode_rows`/`parse_table_map`。原有全 INT 列形态
//! 逐字节不动（e2e 断言零改）；P2 flashback 追加 `*_is` 族（int pk +
//! VAR_STRING 两列表 `d`.`t`），为加性方法、不触碰既有签名。

// 共享夹具：各测试目标只用其子集（e2e 用 INT 族、flashback 用 *_is 族），
// 模块级 allow 消跨 target 的 dead_code（-D warnings 硬闸下的既定形态）。
#![allow(dead_code)]

/// `d`.`t` UPDATE 行对 `(before, after)`（消 clippy::type_complexity；
/// 夹具值恒字面量串，'static 足够）。
pub(crate) type IsPair = ((i32, &'static str), (i32, &'static str));

pub struct Synth {
    pub(crate) bytes: Vec<u8>,
}

impl Synth {
    pub(crate) fn new() -> Self {
        // magic + 手写 FDE（v4、server 8.0.46、alg=0 → 无 checksum）
        let mut s = Synth {
            bytes: b"\xfebin".to_vec(),
        };
        let mut fde = Vec::new();
        fde.extend_from_slice(&4u16.to_le_bytes()); // binlog version
        let mut sv = [0u8; 50];
        sv[.."8.0.46".len()].copy_from_slice(b"8.0.46");
        fde.extend_from_slice(&sv);
        fde.extend_from_slice(&1600000000u32.to_le_bytes()); // create ts
        fde.push(19); // common header length
        fde.extend_from_slice(&[27u8; 39]); // event type header lengths（占位）
        fde.push(0); // checksum alg = NONE
        s.push(15, 1000, &fde);
        s
    }

    /// 追加一个事件（无 CRC），返回 (start_pos, end_pos)。
    pub(crate) fn push(&mut self, evtype: u8, ts: u32, body: &[u8]) -> (u32, u32) {
        let size = 19 + body.len() as u32;
        let start = self.bytes.len() as u32;
        let end = start + size;
        let mut b = Vec::new();
        b.extend_from_slice(&ts.to_le_bytes()); // timestamp
        b.push(evtype); // type code
        b.extend_from_slice(&9u32.to_le_bytes()); // server_id
        b.extend_from_slice(&size.to_le_bytes()); // event_size
        b.extend_from_slice(&end.to_le_bytes()); // log_pos
        b.extend_from_slice(&0x01u16.to_le_bytes()); // flags: BINLOG_IN_USE
        b.extend_from_slice(body);
        debug_assert_eq!(b.len(), size as usize);
        self.bytes.extend_from_slice(&b);
        (start, end)
    }

    /// QUERY 事件（BEGIN / 其它），body 布局对照 file_reader::query_text。
    pub(crate) fn query(&mut self, db: &str, sql: &str, ts: u32) -> (u32, u32) {
        let mut b = Vec::new();
        b.extend_from_slice(&0u32.to_le_bytes()); // proxy_db_id
        b.extend_from_slice(&0u32.to_le_bytes()); // exec_time
        b.push(db.len() as u8); // schema_len
        b.extend_from_slice(&0u16.to_le_bytes()); // error code
        b.extend_from_slice(&0u16.to_le_bytes()); // status_vars_len
        b.extend_from_slice(db.as_bytes());
        b.push(0); // schema NUL
        b.extend_from_slice(sql.as_bytes());
        self.push(2, ts, &b)
    }

    pub(crate) fn xid(&mut self, ts: u32) -> (u32, u32) {
        self.push(16, ts, &42u64.to_le_bytes())
    }

    /// TABLE_MAP：`n` 个 INT（type 3）列，metadata 段 0 字节。
    pub(crate) fn table_map(
        &mut self,
        tid: u64,
        db: &str,
        tb: &str,
        n: usize,
        ts: u32,
    ) -> (u32, u32) {
        let mut b = Vec::new();
        b.extend_from_slice(&tid.to_le_bytes()[..6]);
        b.extend_from_slice(&0u16.to_le_bytes()); // flags
        b.push(db.len() as u8);
        b.extend_from_slice(db.as_bytes());
        b.push(0);
        b.push(tb.len() as u8);
        b.extend_from_slice(tb.as_bytes());
        b.push(0);
        b.push(n as u8); // n_cols (LNE < 251)
        b.extend(std::iter::repeat_n(3u8, n)); // 每列 type = LONG / INT
        b.push(0); // metadata total length = 0（全 INT）
        b.extend_from_slice(&[0u8; 1]); // null_bits（bit_width(<=8)=1B），值不影响本用例
        self.push(19, ts, &b)
    }

    /// rows 事件体：全列 present、非 NULL，逐行 INT 值。`kind` 决定 type code。
    pub(crate) fn rows(
        &mut self,
        tid: u64,
        kind_rows: u8,
        n: usize,
        rows: &[Vec<i32>],
        ts: u32,
    ) -> (u32, u32) {
        let mut b = Vec::new();
        b.extend_from_slice(&tid.to_le_bytes()[..6]);
        b.extend_from_slice(&0u16.to_le_bytes()); // flags
        b.extend_from_slice(&2u16.to_le_bytes()); // extra_info_len = 2（自含，V2）
        b.push(n as u8); // n_cols
        let bm = if n >= 8 {
            0xFFu8
        } else {
            (1u16 << n) as u8 - 1
        };
        b.push(bm); // cols_present bitmap1（全列）
        for row in rows {
            assert_eq!(row.len(), n);
            b.push(0u8); // null_bits：present<=8 → 1B，全非 NULL
            for &v in row {
                b.extend_from_slice(&v.to_le_bytes());
            }
        }
        self.push(kind_rows, ts, &b)
    }

    pub(crate) fn write(&mut self, tid: u64, n: usize, rows: &[Vec<i32>], ts: u32) -> (u32, u32) {
        self.rows(tid, 30, n, rows, ts) // WRITE_ROWS_V2
    }
    pub(crate) fn delete(&mut self, tid: u64, n: usize, rows: &[Vec<i32>], ts: u32) -> (u32, u32) {
        self.rows(tid, 32, n, rows, ts) // DELETE_ROWS_V2
    }

    /// TABLE_MAP：单列 NEWDECIMAL(precision,scale)（type 246，meta 2B 大端对，
    /// 终审 #1 敌意事件回放用）。
    pub(crate) fn table_map_decimal(
        &mut self,
        tid: u64,
        db: &str,
        tb: &str,
        precision: u8,
        scale: u8,
        ts: u32,
    ) -> (u32, u32) {
        let mut b = Vec::new();
        b.extend_from_slice(&tid.to_le_bytes()[..6]);
        b.extend_from_slice(&0u16.to_le_bytes()); // flags
        b.push(db.len() as u8);
        b.extend_from_slice(db.as_bytes());
        b.push(0);
        b.push(tb.len() as u8);
        b.extend_from_slice(tb.as_bytes());
        b.push(0);
        b.push(1); // n_cols
        b.push(246); // MYSQL_TYPE_NEWDECIMAL
        b.push(2); // metadata 总长
        b.push(precision);
        b.push(scale);
        b.push(0); // null_bits（1 列 → 1B）
        self.push(19, ts, &b)
    }

    /// WRITE_ROWS_V2：单列、行载荷裸字节（行 null 区 1B=非NULL + 给定 payload），
    /// 供敌意 DECIMAL 字节直灌解码层。
    pub(crate) fn write_raw(&mut self, tid: u64, payloads: &[Vec<u8>], ts: u32) -> (u32, u32) {
        let mut b = Vec::new();
        b.extend_from_slice(&tid.to_le_bytes()[..6]);
        b.extend_from_slice(&0u16.to_le_bytes()); // flags
        b.extend_from_slice(&2u16.to_le_bytes()); // extra_info_len = 2（自含）
        b.push(1); // n_cols
        b.push(0x01); // cols_present：列 0
        for p in payloads {
            b.push(0u8); // 行 null_bits（1 列 → 1B，非 NULL）
            b.extend_from_slice(p);
        }
        self.push(30, ts, &b)
    }

    // ---------- P2 T3 追加：flashback fixture 表 `d`.`t`（id INT pk + b VAR_STRING） ----------

    /// TABLE_MAP：两列 `id` INT(3) + `b` VAR_STRING(253, meta=max_len 2B LE)。
    pub(crate) fn table_map_is(
        &mut self,
        tid: u64,
        db: &str,
        tb: &str,
        str_max_len: u16,
        ts: u32,
    ) -> (u32, u32) {
        let mut b = Vec::new();
        b.extend_from_slice(&tid.to_le_bytes()[..6]);
        b.extend_from_slice(&0u16.to_le_bytes()); // flags
        b.push(db.len() as u8);
        b.extend_from_slice(db.as_bytes());
        b.push(0);
        b.push(tb.len() as u8);
        b.extend_from_slice(tb.as_bytes());
        b.push(0);
        b.push(2); // n_cols
        b.push(3); // id：LONG / INT
        b.push(253); // b：VAR_STRING
        b.push(2); // metadata 总长（VAR_STRING 2B LE；INT 0B）
        b.extend_from_slice(&str_max_len.to_le_bytes());
        b.push(0); // null_bits（bit_width(2)=1B）
        self.push(19, ts, &b)
    }

    /// `d`.`t` 单镜像行：null_bits(1B=非NULL) + int LE4 + 串长(1B，max_len<256) + 串字节。
    fn is_row(r: (i32, &str)) -> Vec<u8> {
        let mut v = vec![0u8];
        v.extend_from_slice(&r.0.to_le_bytes());
        v.push(r.1.len() as u8);
        v.extend_from_slice(r.1.as_bytes());
        v
    }

    /// `d`.`t` 事件体头：tid + flags + extra_info_len(2) + n_cols(2) + bm(全列=0b11)。
    fn is_head(tid: u64) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&tid.to_le_bytes()[..6]);
        b.extend_from_slice(&0u16.to_le_bytes()); // flags
        b.extend_from_slice(&2u16.to_le_bytes()); // extra_info_len = 2（自含）
        b.push(2); // n_cols
        b.push(0b11); // cols_present：两列全在
        b
    }

    pub(crate) fn write_is(&mut self, tid: u64, rows: &[(i32, &str)], ts: u32) -> (u32, u32) {
        let mut b = Self::is_head(tid);
        for &r in rows {
            b.extend(Self::is_row(r));
        }
        self.push(30, ts, &b)
    }

    pub(crate) fn delete_is(&mut self, tid: u64, rows: &[(i32, &str)], ts: u32) -> (u32, u32) {
        let mut b = Self::is_head(tid);
        for &r in rows {
            b.extend(Self::is_row(r));
        }
        self.push(32, ts, &b)
    }

    /// UPDATE_ROWS_V2：bm1+bm2 双位图（均全列），每行对 before/after 交错。
    /// `IsRow = (id, b)`；`IsPair = (before, after)`（消 clippy type_complexity）。
    pub(crate) fn update_is(&mut self, tid: u64, pairs: &[IsPair], ts: u32) -> (u32, u32) {
        let mut b = Self::is_head(tid);
        b.push(0b11); // cols_present bitmap2（after 镜像全列）
        for &(before, after) in pairs {
            b.extend(Self::is_row(before));
            b.extend(Self::is_row(after));
        }
        self.push(31, ts, &b)
    }
}
