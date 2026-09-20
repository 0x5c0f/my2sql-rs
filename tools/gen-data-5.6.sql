-- Task 17：5.6 矩阵级裁剪版 gen-data（与 gen-data.sql 逐行同源，仅剔 JSON 单元格）
-- 剔除依据：MySQL 5.6 无 JSON 类型（5.7.8 引入）→ t_all.c_json 列、t_json 专表、
-- 事务内 UPDATE t_json 全部去除；其余 19 组保持与 8.0 矩阵一致（覆盖最大化）。
-- 排除登记：docs/compat/matrix.md（矩阵级排除，非白名单放宽；NOTE ALW-56-JSON 落地）。
-- 注意：改动 gen-data.sql 时本文件必须同步（run-difftest.sh 按 VER 自动选择本文件）。
SET SESSION sql_mode='';            -- 放行零日期/零时间/空 ENUM（矩阵需要）
SET NAMES utf8mb4;
DROP DATABASE IF EXISTS dt;
CREATE DATABASE dt CHARACTER SET utf8mb4;
USE dt;

CREATE TABLE t_all (
  id INT NOT NULL AUTO_INCREMENT PRIMARY KEY,
  c_tiny TINYINT, c_small SMALLINT, c_med MEDIUMINT, c_int INT, c_big BIGINT,
  c_float FLOAT, c_double DOUBLE,
  c_dec DECIMAL(10,2), c_dec65 DECIMAL(65,30),
  c_bit1 BIT(1), c_bit9 BIT(9), c_bit64 BIT(64),
  c_year YEAR,
  c_date DATE, c_dt DATETIME, c_dt3 DATETIME(3), c_dt6 DATETIME(6),
  c_ts TIMESTAMP NULL DEFAULT NULL, c_ts3 TIMESTAMP(3) NULL DEFAULT NULL,
  c_time TIME, c_time3 TIME(3),
  c_char CHAR(10), c_vc VARCHAR(300), c_text TEXT,
  c_tinyblob TINYBLOB, c_blob BLOB, c_mblob MEDIUMBLOB, c_lblob LONGBLOB,
  c_varb VARBINARY(50),
  c_enum ENUM('a','b','c') DEFAULT 'a', c_set SET('s1','s2','s4') DEFAULT 's1'
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- 正常行（emoji、中文、非 UTF-8 blob）
INSERT INTO t_all (c_tiny,c_small,c_med,c_int,c_big,c_float,c_double,c_dec,c_dec65,
  c_bit1,c_bit9,c_bit64,c_year,c_date,c_dt,c_dt3,c_dt6,c_ts,c_ts3,c_time,c_time3,
  c_char,c_vc,c_text,c_tinyblob,c_blob,c_mblob,c_lblob,c_varb,c_enum,c_set) VALUES
  (100,30000,8000000,2000000000,9000000000000000000,
   1.5,-2.25,123.45,'123456789012345678901234567890.12345678901234567890123456789',
   b'1',b'110100110',b'1000100010010010101101001101111000010000100110101011110011011110',
   2020,'2020-02-29','2020-06-01 12:34:56','2020-06-01 12:34:56.123','2020-06-01 12:34:56.123456',
   '2020-06-01 00:00:01','2038-01-19 03:14:07.999',
   '12:34:56','-01:02:03.456',
   'ascii','😀 emoji 😀', CONCAT('中文文本_', REPEAT('数',50)),
   'tiny', X'89504E470D0A1A0A', X'00FF10FE', REPEAT('L',5000), X'DEADBEEF',
   'b','s1,s4');

-- NULL 行
INSERT INTO t_all (c_tiny,c_small,c_med,c_int,c_big,c_float,c_double,c_dec,c_dec65,
  c_bit1,c_bit9,c_bit64,c_year,c_date,c_dt,c_dt3,c_dt6,c_ts,c_ts3,c_time,c_time3,
  c_char,c_vc,c_text,c_tinyblob,c_blob,c_mblob,c_lblob,c_varb,c_enum,c_set) VALUES
  (NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,
   NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL);

-- 零值行（含 TIMESTAMP 零值——白名单①：上游 0000-00-00 vs 本侧 1970-01-01）
INSERT INTO t_all (c_tiny,c_small,c_med,c_int,c_big,c_float,c_double,c_dec,c_dec65,
  c_bit1,c_bit9,c_bit64,c_year,c_date,c_dt,c_dt3,c_dt6,c_ts,c_ts3,c_time,c_time3,
  c_char,c_vc,c_text,c_tinyblob,c_blob,c_mblob,c_lblob,c_varb,c_enum,c_set) VALUES
  (0,0,0,0,0,0,0,0,0,b'0',b'0',b'0',0,
   '0000-00-00','0000-00-00 00:00:00','0000-00-00 00:00:00.000','0000-00-00 00:00:00.000000',
   '0000-00-00 00:00:00','0000-00-00 00:00:00.000',
   '00:00:00','00:00:00.000',
   '','','',
   '','','','','',
   '',0);

-- 边界行（各类型 MIN/MAX；BIT(64) 高位置 1、DECIMAL(65,30) 全 9、TIME >24h）
INSERT INTO t_all (c_tiny,c_small,c_med,c_int,c_big,c_float,c_double,c_dec,c_dec65,
  c_bit1,c_bit9,c_bit64,c_year,c_date,c_dt,c_dt3,c_dt6,c_ts,c_ts3,c_time,c_time3,
  c_char,c_vc,c_text,c_tinyblob,c_blob,c_mblob,c_lblob,c_varb,c_enum,c_set) VALUES
  (-128,-32768,-8388608,-2147483648,-9223372036854775808,
   3.4028235E38,-1.7976931348623157E308,-99999999.99,
   '-99999999999999999999999999999999999.99999999999999999999999999999',
   b'1',b'111111111',b'1111111111111111111111111111111111111111111111111111111111111111',
   2155,'9999-12-31','1000-01-01 00:00:00','9999-12-31 23:59:59.999','1000-01-01 00:00:00.000001',
   '2038-01-19 03:14:08','1970-01-01 00:00:01.000',
   '838:59:59','-838:59:59.999999',
   '0123456789', REPEAT('😀',150), REPEAT('边',3000),
   X'FF', X'FFFFFFFF', X'010203', REPEAT(X'FE',500), X'000102030405',
   'c','s1,s2,s4');

-- unsigned 变体
CREATE TABLE t_unsigned (
  id INT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
  u_tiny TINYINT UNSIGNED, u_small SMALLINT UNSIGNED, u_med MEDIUMINT UNSIGNED,
  u_int INT UNSIGNED, u_big BIGINT UNSIGNED,
  u_dec DECIMAL(10,2) UNSIGNED, u_dec65 DECIMAL(65,30) UNSIGNED,
  c_bin BINARY(8), c_vb VARBINARY(16)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
INSERT INTO t_unsigned VALUES
  (1,100,200,16000000,4000000000,18446744073709551615,123.45,
   '123456789012345678901234567890.12345678901234567890123456789',X'6162630000000000',X'FF00FE'),
  (2,0,0,0,0,0,0,0,X'0000000000000000',X''),
  (3,255,65535,16777215,4294967295,18446744073709551615,99999999.99,
   '99999999999999999999999999999999999.99999999999999999999999999999',
   X'FFFFFFFFFFFFFFFF',X'0123456789ABCDEF'),
  (4,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL);

-- utf8mb3 表（合法 UTF-8 中文字节，两家均应引号文本化）
CREATE TABLE t_utf8 (
  id INT NOT NULL PRIMARY KEY, s VARCHAR(50) CHARACTER SET utf8, t TEXT CHARACTER SET utf8
) ENGINE=InnoDB DEFAULT CHARSET=utf8;
INSERT INTO t_utf8 VALUES (1,'中文utf8','多字节文本测试'), (2,NULL,''), (3,'Ω≈ç√','暂存');

-- gbk 表（GBK 存储字节 = 非 UTF-8 → blob 形态白名单实操区）
CREATE TABLE t_gbk (
  id INT NOT NULL PRIMARY KEY, name VARCHAR(100) CHARACTER SET gbk, note TEXT CHARACTER SET gbk
) ENGINE=InnoDB DEFAULT CHARSET=gbk;
INSERT INTO t_gbk VALUES (1,'中文GBK测试','引号\'与\\反斜杠以及换行\n控制符\t'), (2,'',''), (3,NULL,NULL);

-- 仅 UK 表（无 PK；WHERE 走唯一键，两家同取 uk[0]）
CREATE TABLE t_uk (
  id INT NOT NULL, code VARCHAR(20) NOT NULL, v INT,
  UNIQUE KEY uk_code (code)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
INSERT INTO t_uk VALUES (1,'A001',10),(2,'A002',20),(3,'A003',NULL);

-- 无键表（WHERE 全列等值）
CREATE TABLE t_nokey (
  a INT, b VARCHAR(20), c DATETIME
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
INSERT INTO t_nokey VALUES (1,'x','2020-01-01 00:00:00'),(2,NULL,NULL),(3,'y','2021-06-01 10:00:00');

-- 多行事务（BEGIN + 多语句 + 多行 VALUES 事件 + COMMIT；JSON 专句已随类型剔除）
START TRANSACTION;
INSERT INTO t_all (c_tiny,c_vc) VALUES (1,'trx-a'),(2,'trx-b'),(3,'trx-c');
UPDATE t_all SET c_vc='upd-1' WHERE id=1;
UPDATE t_uk SET v=v+1 WHERE code='A001';
DELETE FROM t_nokey WHERE a=2;
COMMIT;

-- 单语句事务（autocommit 独立事务 ×3，覆盖 INSERT/UPDATE/DELETE 三种）
UPDATE t_utf8 SET s='changed' WHERE id=1;
UPDATE t_gbk SET name='改GBK名' WHERE id=1;
DELETE FROM t_uk WHERE code='A003';

-- 仅 UK / 无键表的按键定位语句
START TRANSACTION;
UPDATE t_uk SET v=99 WHERE code='A002';
UPDATE t_nokey SET b='z' WHERE a=1;
DELETE FROM t_nokey WHERE a=3;
COMMIT;
