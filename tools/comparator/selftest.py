#!/usr/bin/env python3
"""compare.py 自测（brief Step 3 绑定 4 例，含一个故意不等；plain-assert runner，
环境无 pytest）。由 run-difftest.sh 先行调用，红灯即整个差分基建不可信。"""
import sys, os
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from compare import parse_stmt, veq, load, main
import tempfile, subprocess

def P(s):
    return parse_stmt(s)

# 1) JSON：键序（存储序 vs 字典序）、double 文本（12.0 vs 12）、HTML 转义 —— 判等
a = P("INSERT INTO `dt`.`t` (`id`,`j`) VALUES (1,'{\"b\":1,\"aa\":[1,{\"k\":\"<&>\"}],\"n\":12.0}');")
b = P("INSERT INTO `dt`.`t` (`id`,`j`) VALUES (1, '{\"aa\": [1, {\"k\": \"\\u003c&\\u003e\"}], \"b\": 1, \"n\": 12 }');")
assert veq(a, b), "json rule failed"

# 2) decimal 文本/引号形态 + hex 三形态（0xUPPER / X'lower' / 原样字节）按字节判等
a = P("INSERT INTO `d`.`t` (`a`,`b`,`c`) VALUES (1.50,0xFF,'abc');")
b = P("INSERT INTO `d`.`t` (`a`,`b`,`c`) VALUES ('1.5',X'ff','abc');")
assert veq(a, b), "decimal/hex rule failed"

# 3) 故意不等：值不同必须判红（比较器不得退化为存在性检查）
a = P("INSERT INTO `d`.`t` (`x`) VALUES (1);")
b = P("INSERT INTO `d`.`t` (`x`) VALUES (2);")
assert not veq(a, b), "comparator must reject unequal values"
a = P("INSERT INTO `d`.`t` (`x`,`y`) VALUES (1,'aa');")
b = P("INSERT INTO `d`.`t` (`x`,`y`) VALUES (1,'ab');")
assert not veq(a, b), "comparator must reject unequal strings"

# 4) WHERE 括号/大小写/IS null/分隔空白差异 —— 结构判等；BIT64 回绕/零日期对
a = P("UPDATE `d`.`t` SET `v`=2 WHERE (`id`=1 AND `w` IS null);")
b = P("UPDATE d.t SET v = 2 WHERE id=1 AND w IS NULL;")
assert veq(a, b), "where-structure rule failed"
a = P("INSERT INTO `d`.`t` (`b`,`ts`) VALUES (18446744073709551615,'1970-01-01 00:00:00');")
b = P("INSERT INTO `d`.`t` (`b`,`ts`) VALUES (-1,'0000-00-00 00:00:00');")
assert veq(a, b), "int-wrap / zero-date pair rule failed"

# 5) float 宽度（Go f64 展开 vs Rust f32 最短，同 bits 判等）；高精度尾差仍红
a = P("INSERT INTO `d`.`t` (`f`) VALUES (3.4028234663852886e+38);")
b = P("INSERT INTO `d`.`t` (`f`) VALUES (3.4028235e38);")
assert veq(a, b), "float-width rule failed"
a = P("INSERT INTO `d`.`t` (`f`) VALUES (340282346638528860000000000000000000000);")
b = P("INSERT INTO `d`.`t` (`f`) VALUES (340282346638528860000000000000000000001);")
assert not veq(a, b), "high-precision decimal must stay strict"

# 6) 零日期规则含 fsp 后缀；非零值不得被误放
a = P("INSERT INTO `d`.`t` (`ts`) VALUES ('0000-00-00 00:00:00.000');")
b = P("INSERT INTO `d`.`t` (`ts`) VALUES ('1970-01-01 00:00:00.000');")
assert veq(a, b), "zero-timestamp fsp rule failed"
a = P("INSERT INTO `d`.`t` (`ts`) VALUES ('0000-00-00 00:00:00');")
b = P("INSERT INTO `d`.`t` (`ts`) VALUES ('1970-01-01 00:00:01');")
assert not veq(a, b), "zero-date rule must not leak"

# 7) ALW-FLOAT-WIDTH 类型盲修复守卫：f32 bits 兜底仅当一侧 f32-canonical。
#    DECIMAL(10,2) 相邻 BCD 值（f32 舍入后同 bits）必须判红（真实解码 bug 形态）
a = P("INSERT INTO `d`.`t` (`x`) VALUES (12345678.90);")
b = P("INSERT INTO `d`.`t` (`x`) VALUES (12345678.91);")
assert not veq(a, b), "DECIMAL near-equal pair must stay red (f32-collapse leak)"
a = P("INSERT INTO `d`.`t` (`x`) VALUES (-99999999.99);")
b = P("INSERT INTO `d`.`t` (`x`) VALUES (-99999999.98);")
assert not veq(a, b), "DECIMAL boundary pair must stay red (f32-collapse leak)"
#    真 float 对（Go f64 最短展开 vs Rust f32 最短，一侧 canonical + 同 bits）判绿
a = P("INSERT INTO `d`.`t` (`f`) VALUES (3.14);")
b = P("INSERT INTO `d`.`t` (`f`) VALUES (3.140000104904175);")
assert veq(a, b), "f32-canonical float pair must stay green"

# 8) JSON-in-SET：上游恒在 UPDATE SET 带 JSON 列；非 JSON 多余项/交集不等仍红
a = P("UPDATE `d`.`t` SET `v`=2,`j`='{\"a\":1}' WHERE `id`=1;")
b = P("UPDATE `d`.`t` SET `v`=2 WHERE `id`=1;")
assert veq(a, b), "json-in-set rule failed"
a = P("UPDATE `d`.`t` SET `v`=2,`w`=3 WHERE `id`=1;")
b = P("UPDATE `d`.`t` SET `v`=2 WHERE `id`=1;")
assert not veq(a, b), "non-json extra SET entry must stay red"
a = P("UPDATE `d`.`t` SET `v`=2,`j`='{\"a\":2}' WHERE `id`=1;")
b = P("UPDATE `d`.`t` SET `v`=2,`j`='{\"a\":1}' WHERE `id`=1;")
assert not veq(a, b), "JSON SET intersection must stay strict"

print("comparator selftest: 8/8 groups (4 brief cases + float-width guard + rule extensions) OK")
