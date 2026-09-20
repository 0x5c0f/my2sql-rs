#!/usr/bin/env python3
"""Task 15 语义差分器：Go 裁判 vs my2sql-rs to-sql 输出。按 extra-info
(binlog,startpos,stoppos) 对齐语句组；VALUES/WHERE/SET 字面量解析为值序列判等。
规则权威 = tools/difftest-allowlist.txt；自测 = selftest.py。
环境无 sqlparse（pip 不可用），按预案手写解析（stdlib only）。"""
import sys, re, json, glob, os, struct
from decimal import Decimal, InvalidOperation  # noqa
HDR = re.compile(r"^# datetime=\S+ database=\S+ table=\S+ binlog=(\S+) startpos=(\d+) stoppos=(\d+)$")
HEXP = re.compile(r"^0[xX][0-9a-fA-F]*$"); XHQ = re.compile(r"^X'([0-9a-fA-F]*)'$", re.I)
NUMP = re.compile(r"^[+-]?(\d+(\.\d*)?|\.\d+)([eE][+-]?\d+)?$"); TAGS = ("null", "bytes", "text", "num", "raw")
ZERO_A, ZERO_B = "0000-00-00 00:00:00", "1970-01-01 00:00:00"
def _scan(s):  # 迭代引号('...'/`...`)外的位置；\\ 与 doubled-quote 转义均尊重
    q, i, n = None, 0, len(s)
    while i < n:
        c = s[i]
        if q:
            if c == "\\" and q == "'": i += 2; continue
            if c == q:
                if i + 1 < n and s[i + 1] == q: i += 2; continue
                q = None
            i += 1; continue
        elif c in "'`": q = c
        yield i, c; i += 1
def split_top(s, sep=","):
    out, d, last = [], 0, 0
    for i, c in _scan(s):
        if c == "(": d += 1
        elif c == ")": d -= 1
        elif c == sep and d == 0: out.append(s[last:i].strip()); last = i + 1
    if s[last:].strip(): out.append(s[last:].strip())
    return out
def find_kw(s, kw, noparen=False):
    up, d = s.upper(), 0
    for i, c in _scan(s):
        if c == "(": d += 1
        elif c == ")": d -= 1
        elif (d == 0 or noparen) and up.startswith(kw, i): return i
    return -1
def split_kw(s, kw, noparen=False):
    parts, rest = [], s
    while (i := find_kw(rest, kw, noparen)) >= 0:
        parts.append(rest[:i]); rest = rest[i + len(kw):]
    return parts + [rest]
def unq(v):
    s, out, i, n = v[1:-1], [], 0, len(v) - 2
    esc = {"0": "\0", "n": "\n", "r": "\r", "t": "\t", "b": "\b", "Z": "\x1a", "\\": "\\", "'": "'", '"': '"', "%": "%", "_": "_"}
    while i < n:
        c = s[i]
        if c == "\\" and i + 1 < n: out.append(esc.get(s[i + 1], "\\" + s[i + 1])); i += 2
        elif c == "'" and i + 1 < n and s[i + 1] == "'": out.append("'"); i += 2
        else: out.append(c); i += 1
    return "".join(out)
def canon(v):
    v = v.strip(); u = v.upper()
    if u == "NULL": return ("null",)
    if (m := XHQ.match(v)): return ("bytes", bytes.fromhex(m.group(1)))
    if HEXP.match(v): return ("bytes", bytes.fromhex(v[2:]))
    if v[:1] == "'": return ("text", unq(v))
    if NUMP.match(u):
        try: return ("num", Decimal(u))
        except InvalidOperation: pass
    return ("raw", v)
def jload(s):
    if s[:1] in ("{", "["):
        try: return json.loads(s)
        except Exception: pass
def jeq(x, y):  # JSON deep-equal: key order / 12 vs 12.0 / int-uint tolerated
    if isinstance(x, bool) or isinstance(y, bool): return type(x) is type(y) and x == y
    if isinstance(x, (int, float)) and isinstance(y, (int, float)): return Decimal(str(x)) == Decimal(str(y))
    if isinstance(x, dict) and isinstance(y, dict): return x.keys() == y.keys() and all(jeq(x[k], y[k]) for k in x)
    if isinstance(x, list) and isinstance(y, list): return len(x) == len(y) and all(jeq(a, b) for a, b in zip(x, y))
    return x == y
def sig(d): return len("".join(map(str, d.as_tuple().digits)).rstrip("0") or "0")  # 有效数字位数
def eq(a, b):
    if a == b: return True
    if a[0] == "text" and b[0] == "bytes": return a[1].encode("utf-8", "surrogateescape") == b[1]
    if a[0] == "bytes" and b[0] == "text": return a[1] == b[1].encode("utf-8", "surrogateescape")
    if a[0] == "text" and b[0] == "text":
        if sorted((a[1][:19], b[1][:19])) == [ZERO_A, ZERO_B] and a[1][19:] == b[1][19:]: return True  # ALW-ZERO-TIMESTAMP(含fsp)
        ja, jb = jload(a[1]), jload(b[1]); return bool(ja is not None and jb is not None and jeq(ja, jb))
    if a[0] in ("num", "text") and b[0] in ("num", "text"):
        try: dx, dy = Decimal(str(a[1]).upper()), Decimal(str(b[1]).upper())
        except InvalidOperation: return False
        if dx == dy: return True
        try:
            if dx == dx.to_integral_value() and dy == dy.to_integral_value() and int(dx) % (1 << 64) == int(dy) % (1 << 64): return True  # ALW-INT-SIGN-WRAP
            if sig(dx) > 17 or sig(dy) > 17: return False  # 高精度(DECIMAL65)保持严格
            # ALW-FLOAT-WIDTH: Go 把 f32 值按 f64 最短展开、Rust 按 f32 最短打印。
            # 仅当一侧文本自身就是其 f32 值的精确 f64 还原(f32-canonical, 真 float 的
            # 展开侧必满足)且两侧 pack 成同一 f32 bits 才容忍；DECIMAL 相邻值
            # (如 12345678.90/.91)两侧均非 canonical、f32 舍入同 bits 也不放行。
            fx, fy = float(a[1]), float(b[1])
            bx, by = struct.pack("!f", fx), struct.pack("!f", fy)
            return bx == by and (struct.unpack("!f", bx)[0] == fx or struct.unpack("!f", by)[0] == fy)
        except (InvalidOperation, ValueError, OverflowError): return False
    return False
def jsonish(c): return c[0] == "text" and isinstance(jload(c[1]), (dict, list))
def seteq(sa, sb):  # ALW-JSON-IN-SET: 上游UPDATE SET恒含JSON列；多出项须为JSON才容忍，交集严格
    da, db = {c: v for c, o, v in sa}, {c: v for c, o, v in sb}
    if any(not jsonish(x[k]) for x, y in ((da, db), (db, da)) for k in set(x) - set(y)): return False
    return all(eq(da[k], db[k]) for k in set(da) & set(db))
def veq(x, y):
    if isinstance(x, tuple) and isinstance(y, tuple) and x and y:
        if x[0] in TAGS and y[0] in TAGS: return eq(x, y)
        if x[0] == "upd" == y[0] and len(x) == 4: return x[1] == y[1] and seteq(x[2], y[2]) and veq(x[3], y[3])
        return len(x) == len(y) and all(veq(a, b) for a, b in zip(x, y))
    return x == y
def idn(s):
    return re.sub(r"[`\s()]", "", s).lower()
def cond(a):
    a = a.strip().strip("() ")
    if (m := re.match(r"(?is)^(.*?)\s+IS\s+(NOT NULL|NULL)$", a)): return (idn(m.group(1)), "is", m.group(2).upper())
    k, v = split_top(a, "=")
    return (idn(k), "=", canon(v))
def parse_stmt(s):
    s = s.strip().rstrip(";").strip(); u = s.upper()
    if u.startswith("INSERT"):
        i = find_kw(s, " VALUES "); t, cols = s[:i].split("(", 1)
        return ("ins", idn(re.split(r"(?i)\s+INTO\s+", t, 1)[1]),
                tuple(idn(c) for c in split_top(cols[:-1])),
                tuple(canon(v) for v in split_top(s[i + 8:][1:-1])))
    if u.startswith("UPDATE"):
        si, wi = find_kw(s, " SET "), find_kw(s, " WHERE ")
        return ("upd", idn(s[7:si]), tuple(cond(x) for x in split_top(s[si + 5:wi])),
                tuple(sorted((cond(x) for x in split_kw(s[wi + 7:], " AND ", True)), key=repr)))
    if u.startswith("DELETE"):
        wi = find_kw(s, " WHERE ")
        return ("del", idn(s[12:wi]), tuple(sorted((cond(x) for x in split_kw(s[wi + 7:], " AND ", True)), key=repr)))
    raise ValueError("unknown stmt: " + s[:80])
def load(d):
    g = {}
    for fn in sorted(glob.glob(os.path.join(d, "*.sql"))):
        for line in (l.decode("utf-8", "surrogateescape").rstrip("\r\n") for l in open(fn, "rb")):
            if line.startswith("# datetime="):
                if not (m := HDR.match(line)): raise ValueError("bad extra-info: " + line[:80])
                g.setdefault((m.group(1), int(m.group(2)), int(m.group(3))), [])
            elif line.strip() and not line.upper().startswith("SET NAMES"):
                g[list(g)[-1]].append(parse_stmt(line))
    return g
def main():
    A, B = load(sys.argv[1]), load(sys.argv[2])
    ka, kb, red, green = set(A), set(B), len(set(A) ^ set(B)), 0
    for k in sorted(ka ^ kb): print("ONLY-IN-" + ("A" if k in ka else "B"), k, str((A if k in ka else B)[k][:1])[:160])
    for k in sorted(ka & kb):
        sa, sb, taken = A[k], B[k], set()
        for x in sa: m = [j for j, y in enumerate(sb) if j not in taken and veq(x, y)]; taken.add(m[0] if m else -1)
        if len(sa) != len(sb) or -1 in taken:
            red += 1; print("DIFF", k, str(sa)[:220], " ||| ", str(sb)[:220])
        else: green += 1
    print(f"groups A={len(ka)} B={len(kb)} aligned={len(ka & kb)} green={green} red={red}")
    return 1 if red else 0
if __name__ == "__main__": sys.exit(main())
