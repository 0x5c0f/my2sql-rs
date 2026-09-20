source: docker mysql:8.0.46 default config (binlog_row_metadata=MINIMAL)
capture: T9 implementer session 2026-09-20, table w/ string/unsigned/geometry-ish cols
evidence for: event-code table fix (v2 rows=30/31/32), 8.0 TLV opt-meta relaxation (fields #1 #2 #7)
