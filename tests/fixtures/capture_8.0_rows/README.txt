source: docker mysql:8.0.46 (default binlog_checksum=CRC32, binlog_rows_query_log_events=ON)
captured: T10 implementer session 2026-09-20 (container my2sql-t10probe, db t10)

binlog.000002  FULL-image session. CREATE TABLE u (a INT NOT NULL, b VARCHAR(8)
               NOT NULL, c..h INT NULL, i2 INT NULL) [table_id 85];
               INSERT (30) 1 row [1,'ab',10,NULL,30,40,NULL,60,70];
               UPDATE (31) before above -> [2,'xyz',NULL,11,NULL,0,99,NULL,-5].
               Row region 70B consumed exactly by per-image byte-aligned
               null-bits layout (disproves "shared cursor across images").
binlog.000003  MINIMAL-image session (binlog_row_image=MINIMAL), same table u:
               UPDATE#1 (31) after-image carries ONLY col5 (f=123): cols_bitmap2
               = 20 00, its null-bits region is 1 byte (bit_width(1));
               UPDATE#2 (31) FULL before/after pair (c=55).
binlog.000004  CREATE TABLE j (id INT PRIMARY KEY, doc JSON NOT NULL,
               tag VARCHAR(4) NULL) [table_id 87]:
               WRITE_ROWS_V2 (30)   [1, {"a":1,"b":[1,2]}, NULL];
               PARTIAL_UPDATE_ROWS_V2 (39) x2 (json_set $.a=9; tag='tt')
                 -- T12 routing must never feed these to decode_rows; tests
                 pin that they hard-error when misrouted;
               DELETE_ROWS_V2 (32)  [1, {"a":9,"b":[1,2]}, 'tt'].
               ROWS_QUERY text is a SEPARATE event type 29 (extra_info_len of
               rows events is always 2 = empty section on this server).

Consumed by: src/binlog/rows.rs fixture end-to-end tests (Task 10).
reference/ untouched; parsers/SQL scripts lived in session-scoped /tmp/t10probe.
