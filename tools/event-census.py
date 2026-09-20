#!/usr/bin/env python3
"""T17 修复轮 1：binlog 事件类型普查（V1ROWS 用例实证门）。

逐事件走读 binlog 文件（v4 公共头 19B：type 字节在头内偏移 4，event_size
在偏移 9..13 小端），输出事件类型计数与本方关注的 rows 事件汇总：
  V1 rows = 23/24/25（WRITE/UPDATE/DELETE_ROWS_EVENT_V1）
  V2 rows = 30/31/32（WRITE/UPDATE/DELETE_ROWS_EVENT_V2）
  V0 rows = 20/21/22（5.6.51+ 无开关可产，如出现即事实错误）
（编号对照 MySQL event_codes.h，与本仓库 file_reader.rs 路由表一致。）

用法: event-census.py <binlog-file> [--assert-v1-only]
  --assert-v1-only: 23/24/25 任一为 0 或 30/31/32 任一非 0 → 退出码 1
                    （log_bin_use_v1_row_events=1 服务器的硬断言）。
纯 stdlib，不依赖 mysqlbinlog（官方 minimal 镜像无该二进制）。
"""
import struct
import sys
from collections import Counter

# 关注集 + 常见噪声事件命名（未列类型打 TYPE_<n>，计数照记）
NAMES = {
    1: "START", 2: "QUERY", 3: "STOP", 4: "ROTATE", 5: "INTVAR",
    6: "LOAD", 7: "SLAVE", 8: "CREATE_FILE", 9: "APPEND_BLOCK",
    10: "EXEC_LOAD", 11: "DELETE_FILE", 12: "NEW_LOAD", 13: "RAND",
    14: "USER_VAR", 15: "FORMAT_DESCRIPTION", 16: "XID",
    17: "BEGIN_LOAD_QUERY", 18: "EXECUTE_LOAD_QUERY", 19: "TABLE_MAP",
    20: "WRITE_ROWS_V0", 21: "UPDATE_ROWS_V0", 22: "DELETE_ROWS_V0",
    23: "WRITE_ROWS_V1", 24: "UPDATE_ROWS_V1", 25: "DELETE_ROWS_V1",
    26: "INCIDENT", 27: "HEARTBEAT", 28: "IGNORABLE", 29: "ROWS_QUERY",
    30: "WRITE_ROWS_V2", 31: "UPDATE_ROWS_V2", 32: "DELETE_ROWS_V2",
    33: "GTID_LOG", 34: "ANONYMOUS_GTID_LOG", 35: "PREVIOUS_GTIDS",
    36: "TRANSACTION_CONTEXT", 37: "VIEW_CHANGE", 38: "XA_PREPARE",
    39: "PARTIAL_UPDATE_ROWS", 40: "TRANSACTION_PAYLOAD", 41: "HEARTBEAT_V2",
}
V1, V2, V0 = (23, 24, 25), (30, 31, 32), (20, 21, 22)


def main() -> int:
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    flags = [a for a in sys.argv[1:] if a.startswith("--")]
    if len(args) != 1 or set(flags) - {"--assert-v1-only"}:
        print(__doc__, file=sys.stderr)
        return 2
    path, v1only = args[0], "--assert-v1-only" in flags
    data = open(path, "rb").read()
    if data[:4] != b"\xfebin":
        print(f"CENSUS FAIL {path}: missing fe'bin' magic", file=sys.stderr)
        return 1
    counts, pos = Counter(), 4
    while pos + 19 <= len(data):
        etype = data[pos + 4]
        (size,) = struct.unpack_from("<I", data, pos + 9)
        if size < 19 or pos + size > len(data):
            print(f"CENSUS FAIL {path}: bogus event_size={size} at offset {pos}",
                  file=sys.stderr)
            return 1
        counts[etype] += 1
        pos += size
    if pos != len(data):
        print(f"CENSUS FAIL {path}: {len(data) - pos} trailing bytes", file=sys.stderr)
        return 1

    print(f"# binlog event census: {path}")
    print(f"# total events: {sum(counts.values())}, bytes: {len(data)}")
    for t in sorted(counts):
        print(f"type {t:3d} {NAMES.get(t, 'TYPE_' + str(t)):24s} count {counts[t]}")
    for label, grp in (("V0", V0), ("V1", V1), ("V2", V2)):
        w, u, d = (counts.get(t, 0) for t in grp)
        print(f"rows-{label} (W/U/D) = {w}/{u}/{d}")

    if v1only:
        missing = [t for t in V1 if counts.get(t, 0) == 0]
        leaked = [t for t in V2 if counts.get(t, 0) != 0]
        if missing or leaked:
            print(f"CENSUS FAIL: v1-missing={missing} v2-leaked={leaked}",
                  file=sys.stderr)
            return 1
        print("CENSUS OK: rows events are V1-only (23/24/25 present, 30/31/32 zero)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
