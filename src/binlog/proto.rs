//! 协议原语：length-encoded 整数/字符串 读取 + NULL bitmap 游标。
//!
//! 语义对照 MySQL `proto::row_event.go`（go-mysql `replication/row_event.go` 顶部的
//! `LengthEncodedInt` / `LengthEncodedString` 与 null bitmap 位序）：
//! - 首字节 <0xFB：直接是该值；0xFB=NULL 哨兵（本函数按数值 251 返回，由调用方判 NULL）；
//! - 0xFC/0xFD/0xFE：后续分别跟 2/3/8 字节小端长度；0xFF：非法。
//!
//! bitmap 位序为 LSB first（bit i 位于 `bits[i/8]` 的第 `i%8` 位），bit==1 表示 NULL。

// 骨架阶段本模块尚无生产消费者（Task 5-10 接入），参照 Task 1/2 允许死代码。
#![allow(dead_code)]

use super::error::BinlogError;

/// 读取 length-encoded 整数（1/2/3/8 字节前缀），成功后 `pos` 前进到值之后。
///
/// 对应 go-mysql `LengthEncodedInt`：0xFB 在此按原始数值 251 返回（NULL 哨兵的
/// 语义判定留给行解码层），0xFF 视为非法数据，字节不足报 [`BinlogError::TooShort`]。
pub fn read_lne(buf: &[u8], pos: &mut usize) -> Result<u64, BinlogError> {
    let first = *buf.get(*pos).ok_or(BinlogError::TooShort)?;
    if first < 0xFB {
        // 单字节值本身（0x00..=0xFA）
        *pos += 1;
        return Ok(first as u64);
    }
    let (width, ret): (usize, u64) = match first {
        0xFB => {
            *pos += 1;
            return Ok(251);
        }
        0xFC => {
            let s = buf.get(*pos + 1..*pos + 3).ok_or(BinlogError::TooShort)?;
            (2, u16::from_le_bytes(s.try_into().unwrap()) as u64)
        }
        0xFD => {
            let s = buf.get(*pos + 1..*pos + 4).ok_or(BinlogError::TooShort)?;
            (3, u32::from_le_bytes([s[0], s[1], s[2], 0]) as u64)
        }
        0xFE => {
            let s = buf.get(*pos + 1..*pos + 9).ok_or(BinlogError::TooShort)?;
            (8, u64::from_le_bytes(s.try_into().unwrap()))
        }
        _ => {
            return Err(BinlogError::InvalidData(
                "0xFF is not a valid length-encoded integer prefix".into(),
            ));
        }
    };
    *pos += 1 + width;
    Ok(ret)
}

/// 读取 length-encoded 字符串：先 [`read_lne`] 取长度，再返回该长度切片并推进 `pos`。
///
/// 对应 go-mysql `LengthEncodedString`。
pub fn read_lns<'a>(buf: &'a [u8], pos: &mut usize) -> Result<&'a [u8], BinlogError> {
    let len = read_lne(buf, pos)? as usize;
    let s = buf.get(*pos..*pos + len).ok_or(BinlogError::TooShort)?;
    *pos += len;
    Ok(s)
}

/// `n_cols` 列的 NULL bitmap 字节数（向上取整），对应 go-mysql `bitmapByteSize`。
pub fn bit_width(n_cols: usize) -> usize {
    n_cols.div_ceil(8)
}

/// NULL bitmap 位游标：每次 [`BitmapCursor::next_is_null`] 读取当前位并前进 1 位。
///
/// bit==1 表示 NULL；游标按读取次数连续推进，**不**按行重置——行边界由调用方
/// 自行控制（每行读取 `n_cols` 次，或按 `bit_width*8` 对齐后再建/用游标）。
/// 位序 LSB first：第 i 位取自 `bits[i/8]` 的 bit `i%8`。
pub struct BitmapCursor<'a> {
    bits: &'a [u8],
    bit_width: usize,
    pos: usize,
}

impl<'a> BitmapCursor<'a> {
    /// 以 `bits` 为底层数据、每行位宽 `bit_width`（字节数）构造游标，起始位为 0。
    pub fn new(bits: &'a [u8], bit_width: usize) -> Self {
        Self { bits, bit_width, pos: 0 }
    }

    /// 读取当前位并前进 1 位；返回 true 表示该列 NULL。
    /// 位下标超出 `bits` 长度时返回 false（视为非 NULL，仍推进，由上层校验截断）。
    pub fn next_is_null(&mut self) -> bool {
        let byte = self.pos / 8;
        let bit = self.pos % 8;
        self.pos += 1;
        match self.bits.get(byte) {
            Some(b) => (b >> bit) & 1 == 1,
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_lne_one_byte_values() {
        // 首字节 <0xFB 即值本身
        assert_eq!(read_lne(&[0x07], &mut 0).unwrap(), 7);
        // 0xFB：NULL 哨兵，按数值 251 返回（brief 指定）
        assert_eq!(read_lne(&[0xFB], &mut 0).unwrap(), 251);
    }

    #[test]
    fn read_lne_two_byte_prefix() {
        assert_eq!(read_lne(&[0xFC, 0x34, 0x12], &mut 0).unwrap(), 0x1234);
    }

    #[test]
    fn read_lne_three_byte_prefix() {
        assert_eq!(
            read_lne(&[0xFD, 0x01, 0x02, 0x03], &mut 0).unwrap(),
            0x0003_0201
        );
    }

    #[test]
    fn read_lne_eight_byte_prefix() {
        let mut buf = vec![0xFEu8];
        buf.extend_from_slice(&0x0102_0304_0506_0708u64.to_le_bytes());
        assert_eq!(read_lne(&buf, &mut 0).unwrap(), 0x0102_0304_0506_0708);
    }

    #[test]
    fn read_lne_advances_pos() {
        let mut pos = 1usize;
        assert_eq!(read_lne(&[0x00, 0xFC, 0xAB, 0xCD], &mut pos).unwrap(), 0xCDAB);
        assert_eq!(pos, 4);
    }

    #[test]
    fn read_lne_short_buffer_is_too_short() {
        assert_eq!(read_lne(&[0xFC, 0x01], &mut 0).unwrap_err(), BinlogError::TooShort);
        assert_eq!(read_lne(&[], &mut 0).unwrap_err(), BinlogError::TooShort);
    }

    #[test]
    fn read_lne_0xff_is_invalid() {
        assert!(matches!(
            read_lne(&[0xFF, 0, 0, 0, 0, 0, 0, 0, 0, 0], &mut 0),
            Err(BinlogError::InvalidData(_))
        ));
    }

    #[test]
    fn read_lns_returns_slice_and_advances() {
        let buf = [0x03u8, b'a', b'b', b'c', 0x00];
        let mut pos = 0;
        assert_eq!(read_lns(&buf, &mut pos).unwrap(), b"abc");
        assert_eq!(pos, 4);
    }

    #[test]
    fn read_lns_short_payload_is_too_short() {
        let buf = [0x05u8, b'a', b'b'];
        let mut pos = 0;
        assert_eq!(read_lns(&buf, &mut pos).unwrap_err(), BinlogError::TooShort);
    }

    #[test]
    fn bit_width_rounds_up() {
        assert_eq!(bit_width(1), 1);
        assert_eq!(bit_width(8), 1);
        assert_eq!(bit_width(9), 2);
        assert_eq!(bit_width(0), 0);
    }

    /// brief Step1：8 列 bitmap `[0b10000001]`（列0、列7 为 NULL），
    /// 随后第二行第 0 bit 落入新字节——断言两行共 9 次读取序列。
    #[test]
    fn bitmap_cursor_crosses_bytes_continuously() {
        // 行1：bit0=1(列0 NULL)，bit7=1(列7 NULL)，其余 0；
        // 行2 的列0 落在 byte1 的 bit0（=0 非 NULL），列1 在 byte1 bit1（=1 NULL）。
        let bits = [0b1000_0001u8, 0b0000_0010u8];
        let mut cur = BitmapCursor::new(&bits, bit_width(8));
        // 行 1 的 8 次读取
        assert_eq!(
            (0..8).map(|_| cur.next_is_null()).collect::<Vec<bool>>(),
            vec![true, false, false, false, false, false, false, true]
        );
        // 行 2 第 0 次读取：连续推进到新字节 bit0，不因换行重置
        assert!(!cur.next_is_null());
        // 行 2 第 1 次读取：byte1 bit1 == 1 → NULL
        assert!(cur.next_is_null());
    }

    #[test]
    fn bitmap_cursor_out_of_range_reads_false() {
        let bits = [0b0000_0001u8];
        let mut cur = BitmapCursor::new(&bits, bit_width(8));
        for _ in 0..8 {
            cur.next_is_null();
        }
        assert!(!cur.next_is_null()); // 第 9 位越界 → false，不 panic
    }
}
