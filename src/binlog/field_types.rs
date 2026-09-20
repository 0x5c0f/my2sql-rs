//! `enum_field_types` 列类型码的**唯一**常量表（全 binlog 层共用）。
//!
//! 数值逐一对照 MySQL 官方 `include/mysql_com.h` 的 `enum_field_types`
//! 与 go-mysql `mysql/const.go:102-140`（iota 序）：FLOAT=4/DOUBLE=5、
//! DATE=10/TIME=11/DATETIME=12、TIMESTAMP2=17/DATETIME2=18/TIME2=19、
//! JSON=245、NEWDECIMAL=246、ENUM=247、SET=248、BLOB 家族 249-252、
//! VAR_STRING=253、STRING=254、GEOMETRY=255。
//!
//! 历史沿革（T10 裁定，Step 0）：T5 `int.rs`、T9 `value.rs` 各留一份私有
//! `mod tp` 子集；T4 `table_map.rs` 的第三份把 FLOAT/DOUBLE 与时间 2 族的
//! **名字**相对官方值互换（其 match 三族同走 1B meta，行为无误但命名错误，
//! T5/T9 评审挂账）。本模块以官方值+官方名为准，三处一并并入。

// 常量子集按模块消费（如 DECIMAL/NEWDATE 仅测试穷举用），bin crate 视角下
// 未引用项会报死代码——单一全量表设计使然，参照其余骨架模块豁免。
#![allow(dead_code)] // 完整参考表：未消费码为 P2/穷举预留

pub const DECIMAL: u8 = 0;
pub const TINY: u8 = 1;
pub const SHORT: u8 = 2;
pub const LONG: u8 = 3;
/// 官方 4（table_map.rs 旧私有表误名为 DOUBLE）。
pub const FLOAT: u8 = 4;
/// 官方 5（table_map.rs 旧私有表误名为 FLOAT）。
pub const DOUBLE: u8 = 5;
pub const NULL: u8 = 6;
pub const TIMESTAMP: u8 = 7;
pub const LONGLONG: u8 = 8;
pub const INT24: u8 = 9;
pub const DATE: u8 = 10;
pub const TIME: u8 = 11;
pub const DATETIME: u8 = 12;
pub const YEAR: u8 = 13;
pub const NEWDATE: u8 = 14;
pub const VARCHAR: u8 = 15;
pub const BIT: u8 = 16;
/// 官方 17（table_map.rs 旧私有表误名为 TIME2）。
pub const TIMESTAMP2: u8 = 17;
/// 官方 18（table_map.rs 旧私有表误名为 DATETIME2）。
pub const DATETIME2: u8 = 18;
/// 官方 19（table_map.rs 旧私有表误名为 TIMESTAMP2）。
pub const TIME2: u8 = 19;
pub const JSON: u8 = 0xF5;
pub const NEWDECIMAL: u8 = 0xF6;
pub const ENUM: u8 = 0xF7;
pub const SET: u8 = 0xF8;
pub const TINY_BLOB: u8 = 0xF9;
pub const MEDIUM_BLOB: u8 = 0xFA;
pub const LONG_BLOB: u8 = 0xFB;
pub const BLOB: u8 = 0xFC;
pub const VAR_STRING: u8 = 0xFD;
pub const STRING: u8 = 0xFE;
pub const GEOMETRY: u8 = 0xFF;
