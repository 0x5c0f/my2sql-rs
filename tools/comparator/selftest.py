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
#    残留漏洞守卫：一侧恰好 f32 精确可表示(≤7位短文本)时，canonical 侧必须自身
#    携带 >7 位有效数字（真 Go 展开必满足），否则 DECIMAL/BIGINT 相邻值会漏绿
for ta, tb in (("16777216", "16777217"), ("12345679.0", "12345678.9"),
               ("8388609.0", "8388608.6"), ("16777216", "16777216.5")):
    a = P(f"INSERT INTO `d`.`t` (`x`) VALUES ({ta});")
    b = P(f"INSERT INTO `d`.`t` (`x`) VALUES ({tb});")
    assert not veq(a, b), f"low-precision canonical leak: {ta} vs {tb} must stay red"

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

# 9) P2-T7 rollback 模式：A 侧注释漂尾绑定 / B 侧记录原子 + keep-trx scaffold
#    剥离 / B 侧结构断言（正反例）/ 镜像对不跨组误配。
import io, contextlib
from compare import load as _load

def _hdr(s, e):
    # datetime 为下划线形（constvar DATETIME_FORMAT_NOSPACE，与 HDR \S+ 一致）
    return (f"# datetime=2026-09-21_10:00:00 database=dt table=t1 "
            f"binlog=mysql-bin.000002 startpos={s} stoppos={e}")
INS = "INSERT INTO `dt`.`t1` (`id`) VALUES (2);"
DEL = "DELETE FROM `dt`.`t1` WHERE `id`=1;"
UPD = "UPDATE `dt`.`t1` SET `v`=2 WHERE `id`=3;"
# 镜像对（逆向 vs 正向文本）：同 WHERE 列集、SET/WHERE 值互换——须按 key 严格归组
UPD_REV = "UPDATE `dt`.`t1` SET `v`=1 WHERE `id`=3 AND `v`=2;"
UPD_FWD = "UPDATE `dt`.`t1` SET `v`=2 WHERE `id`=3 AND `v`=1;"
K = lambda s, e: ("mysql-bin.000002", s, e)

def _mkdir(files):
    d = tempfile.mkdtemp()
    for name, lines in files.items():
        with open(os.path.join(d, name), "w") as f:
            f.write("\n".join(lines) + "\n")
    return d

def _main_rc(a, b, mode=None):
    argv = ["compare.py", a, b] + ([mode] if mode else [])
    saved, buf = sys.argv, io.StringIO()
    sys.argv = argv
    try:
        with contextlib.redirect_stdout(buf):
            rc = main()
    finally:
        sys.argv = saved
    return rc, buf.getvalue()

# A 侧（Go rollback.2.sql）：tmp 行序整体倒置 → 语句在前、其注释漂尾；
# KeepTrx=false 默认 → 无任何 begin;/commit; scaffold。
A_OK = _mkdir({"rollback.2.sql": [UPD_REV, _hdr(320, 460), INS, _hdr(200, 320), DEL, _hdr(100, 200)]})
# B 侧（我方 flashback.2.sql）：SET NAMES 头 + -- WARNING 头行 + keep-trx
# scaffold（首部悬空 commit 为平价——上游 lastTrxIdx=0 首块必注入）+ 记录原子（注释先行）。
B_OK = _mkdir({"flashback.2.sql": [
    "SET NAMES utf8mb4;", "-- WARNING: skipped 3 events, positions in stderr",
    "commit;", "begin;", _hdr(320, 460), UPD_REV,
    "commit;", "begin;", _hdr(200, 320), INS,
    "commit;", "begin;", _hdr(100, 200), DEL, "commit;"]})
ga, va = _load(A_OK, "rollback")
gb, vb = _load(B_OK, "rollback")
assert va == [] and vb == [], f"legal trees must have no structural violation: {va}{vb}"
assert set(ga) == {K(100, 200), K(200, 320), K(320, 460)} == set(gb), "rb align keys failed"
# scaffold 剥离不误吞真 DELETE：DEL 记录组恰 1 条 del 语句（裸 commit;/begin;
# 与任何真语句文本不可同值，剥离安全）
assert len(gb[K(100, 200)]) == 1 and gb[K(100, 200)][0][0] == "del", "scaffold skip ate DELETE"
assert ga == gb, "A-side drift bind != B-side atomic bind"
rc, _ = _main_rc(A_OK, B_OK, "rollback")
assert rc == 0, "drift-vs-atomic rollback compare must go green"

# 结构断言红例（白名单不吞结构：违例经 main 计入 red → 退出码 1）
B_NOTAIL = _mkdir({"flashback.2.sql": [
    "SET NAMES utf8mb4;",
    "commit;", "begin;", _hdr(320, 460), UPD_REV,
    "commit;", "begin;", _hdr(200, 320), INS,
    "commit;", "begin;", _hdr(100, 200), DEL]})  # 缺尾 commit;
_, v = _load(B_NOTAIL, "rollback")
assert v and any("tail" in w for _, w in v), f"missing tail commit must be red: {v}"
rc, out = _main_rc(A_OK, B_NOTAIL, "rollback")
assert rc == 1 and "STRUCT-RED" in out, "structural red must survive to exit code"
B_BADBEGIN = _mkdir({"flashback.2.sql": [
    "SET NAMES utf8mb4;",
    "commit;", "begin;", _hdr(320, 460), UPD_REV,
    "begin;", _hdr(200, 320), INS,                              # begin 前非 commit
    "commit;", "begin;", _hdr(100, 200), DEL, "commit;"]})
_, v = _load(B_BADBEGIN, "rollback")
assert v and any("preceding" in w for _, w in v), f"begin-without-commit must be red: {v}"
B_BADCOUNT = _mkdir({"flashback.2.sql": [
    "SET NAMES utf8mb4;",
    "commit;", "begin;", _hdr(320, 460), UPD_REV,
    "commit;", "begin;",                                        # 空段：begin(4) != commit-1(3)
    "commit;", "begin;", _hdr(200, 320), INS,
    "commit;", "begin;", _hdr(100, 200), DEL, "commit;"]})
_, v = _load(B_BADCOUNT, "rollback")
assert v and any("count" in w for _, w in v), f"count mismatch must be red: {v}"

# 镜像对不跨组误配：A 两 key 各持逆向形，B 交换两语句体 → 组内判红（无跨 key 兜救）
A_MIRROR = _mkdir({"rollback.2.sql": [UPD_FWD, _hdr(320, 460), UPD_REV, _hdr(100, 200)]})
B_MIRROR = _mkdir({"flashback.2.sql": [
    "SET NAMES utf8mb4;", "commit;", "begin;",
    _hdr(320, 460), UPD_REV,                                    # 与 A 的 320 组文本不同形
    "commit;", "begin;", _hdr(100, 200), UPD_FWD, "commit;"]})
rc, _ = _main_rc(A_MIRROR, B_MIRROR, "rollback")
assert rc == 1, "mirror pair must not be cross-group rescued"
# 同 key 自洽（格式差异由既有规则吸收）→ 绿
A_MIRROR2 = _mkdir({"rollback.2.sql": [UPD_FWD, _hdr(320, 460)]})
B_MIRROR2 = _mkdir({"flashback.2.sql": [
    "SET NAMES utf8mb4;", "commit;", "begin;",
    _hdr(320, 460), "UPDATE dt.t1 SET v = 2 WHERE (id=3 AND v=1);", "commit;"]})
rc, _ = _main_rc(A_MIRROR2, B_MIRROR2, "rollback")
assert rc == 0, "same-key mirror text must stay green"

# A 侧末行孤儿语句（无漂尾注释可绑）→ 解析违例，不得静默丢
A_ORPHAN = _mkdir({"rollback.2.sql": [UPD_REV, _hdr(320, 460), INS]})
_, v = _load(A_ORPHAN, "rollback")
assert v and any("orphan" in w for _, w in v), f"orphan stmt must be red: {v}"

print("comparator selftest: 9/9 groups (4 brief cases + float-width guard + rule extensions + rollback mode) OK")
