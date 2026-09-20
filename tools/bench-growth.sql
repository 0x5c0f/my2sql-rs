-- Task 16 bench 数据放大脚本（tools/gen-bench-binlog.sh 使用；经
-- `mysql --default-character-set=utf8mb4 < 本文件` 灌入 mysql:8.0 容器）。
-- 结构：dt 打底由 gen-data.sql 负责（脚本先行）；本文件建 bench.t_grow
-- （15 列富类型：字符串/文本/BLOB(含非UTF8)/DECIMAL(20,10)/DOUBLE/UNSIGNED/
-- 时间全族 fsp6/JSON/ENUM/SET/BIT(64)/YEAR + NULL/零值/边界/emoji）+
-- 12 行模板 + grow_round() 存储过程（bash 循环调用，每轮 ~12MB binlog）。
-- 每轮 = 3×分批 INSERT…SELECT（模板轮换供给，行宽 ~100-500B 混合）
--       + 全镜像 UPDATE 3000 行 + DELETE 2000 行 → 事件形态 W/U/D 全覆盖。
SET SESSION sql_mode='';           -- 零日期/越界降级放行（模板需要）
CREATE DATABASE IF NOT EXISTS bench;
USE bench;
DROP TABLE IF EXISTS t_grow;
CREATE TABLE t_grow (
  id BIGINT NOT NULL AUTO_INCREMENT PRIMARY KEY,
  s VARCHAR(300), t TEXT, b BLOB,
  d DECIMAL(20,10), f DOUBLE, u INT UNSIGNED,
  dt DATETIME(6), ts TIMESTAMP(3) NULL, tm TIME(6), da DATE, yr YEAR,
  j JSON, e ENUM('a','b','c'), st SET('s1','s2','s4'), bi BIT(64)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- 12 行模板（边界/NULL/emoji/非 UTF-8 字节/深嵌套 JSON/巨大 DECIMAL/负 TIME）
INSERT INTO t_grow (s,t,b,d,f,u,dt,ts,tm,da,yr,j,e,st,bi) VALUES
 ('str-1', REPEAT('长文本',40), REPEAT('x',200), 123456.7890123456789, -2.25, 4000000000,
  '2020-06-01 12:34:56.123456','2038-01-19 03:14:07.999','838:59:59.000000','2020-02-29',2020,
  '{"a":1,"arr":[1,2,{"b":[true,null,"emoji🙂"]}],"d":1234567890.123}',
  'b','s1,s4',64),
 ('🔥炸弹字符串', NULL, UNHEX('00FF80FE'), -0.0000000001, 1e308, 0,
  '0000-00-00 00:00:00.000000', NULL, '-01:02:03.456789','0000-00-00',0,
  '[[1,2,3],"s",{"k":{"嵌套":{"深":[1000000000,1.5e10]}}}]','a','',0),
 ('str-3', REPEAT('z',1), '', 9999999999.9999999999, 0.5, 4294967295,
  '1970-01-01 00:00:01.000001','1970-01-01 00:00:00.000','00:00:00','1000-01-01',1901,
  '{"dec":-0.01,"big":-9223372036854775808,"arr":[]}','c','s2',18446744073709551615),
 (REPEAT('混',80), 'text4', UNHEX('DEADBEEF'), 0.0000000000, -0.0, 7,
  '9999-12-31 23:59:59.999999','2024-02-29 12:00:00.500','24:00:00.5','2024-12-31',2099,
  '{"s":"字符串字符串","嵌套数组":[{"x":1},{"y":[true,false,null,1.5e10]}]}','a','s1,s2,s4',1),
 ('str-5', REPEAT('混',80), UNHEX('C4E3BFAAB2C3C4E3BFAAB2C3'), -12345.6789, 1.7976931348623157e308, 42,
  '2000-01-01 00:00:00','2000-01-01 08:00:00.000','100:00:00.000001','2000-06-15',2000,
  '"裸字符串"','b','s4',NULL),
 (NULL,'t6',REPEAT(0x00,300),1,1,1,'2021-11-23 05:12:34.999999','2021-11-23 05:12:34.000','00:00:01','2021-11-23',2021,'[null,[1,2]]','c','s2,s4',NULL),
 ('str-7','t7','b7',-7,-7,7,'2022-07-07 07:07:07.070707','2022-07-07 07:07:07.007','77:77:07.777777','2022-07-07',2022,'{"k7":7}','a','s1',7),
 (REPEAT('8',250),'t8',REPEAT('b',8),0.8,8.8,8,'1999-12-31 23:59:59.999999','1969-12-31 23:59:59.000','-838:59:59','1969-12-31',1970,'888','b','s2',88),
 ('str-9','t9',0xff,9.9999999999,9,9,'1990-01-01 00:00:00','2038-01-19 03:14:07.000','01:00:00','1990-01-31',1990,'[9,9.9,"九"]','c','s4',9),
 ('字符串十','t10',REPEAT('中',60),10.10,10,10,'2010-10-10 10:10:10.100000','2010-10-10 10:10:10.100','10:10:10.100010','2010-10-10',2010,'["十",10,10.1,true]','a','s1,s2',10),
 ('str-11',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'[]',NULL,NULL,0),
 ('最后一行模板','深','度',-0.5,-5,5,'2019-09-09 09:09:09.090909','2019-09-09 09:09:09.009','99:09:09.090909','2019-09-09',2019,
  JSON_OBJECT('k',JSON_ARRAY(JSON_OBJECT('nested',JSON_ARRAY(1,2,3,'四')),'d',0.0001),'e','emoji🀄'),'b','s1,s4',4096);

DROP PROCEDURE IF EXISTS grow_round;
DELIMITER //
CREATE PROCEDURE grow_round(IN rnd INT)
BEGIN
  SET SESSION sql_mode='';
  -- 三路分批 INSERT（模板轮换：供给行取现存表的 id%4 槽位，行宽 ~100-500B）
  INSERT INTO t_grow (s,t,b,d,f,u,dt,ts,tm,da,yr,j,e,st,bi)
    SELECT CONCAT(COALESCE(s,'x'),'-r',rnd,'-A'),t,b,d,f,u,dt,ts,tm,da,yr,j,e,st,bi
      FROM t_grow WHERE id % 4 = 1 LIMIT 10000;
  INSERT INTO t_grow (s,t,b,d,f,u,dt,ts,tm,da,yr,j,e,st,bi)
    SELECT s,CONCAT(t,'-r',rnd,'-B'),b,d+1.5,f,u,dt,ts,tm,da,yr,j,e,st,bi
      FROM t_grow WHERE id % 4 = 2 LIMIT 10000;
  INSERT INTO t_grow (s,t,b,d,f,u,dt,ts,tm,da,yr,j,e,st,bi)
    SELECT s,t,b,d,f,u,dt,ts,tm,da,yr,
      JSON_SET(COALESCE(j,'{}'), CONCAT('$.r', rnd), rnd * 1.25), e, st, bi
      FROM t_grow WHERE id % 4 = 3 LIMIT 10000;
  -- 全镜像 UPDATE（before+after 双镜像 → 行事件字节翻倍的那类形态）
  UPDATE t_grow
     SET s=CONCAT(COALESCE(s,'u'),'!'), f=f+1.5, d=d+0.001,
         dt=dt+INTERVAL 1 SECOND,
         j=JSON_SET(COALESCE(j,'{}'), '$.u', rnd)
   WHERE bi IS NULL OR bi % 7 < 3
   LIMIT 3000;
  -- DELETE（前像镜像）
  DELETE FROM t_grow WHERE id % 13 = rnd % 13 LIMIT 2000;
END//
DELIMITER ;
