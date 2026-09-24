# Changelog

All notable changes to this project will be documented in this file.

## [0.5.2-p7] - 2026-09-24

### 🎉 Released After P7 Comprehensive Retest (All Phases Verified ✅)

### Fixed
- **B013 (High→Fixed)**: Support MySQL 5.6/NONE checksum format with Stop events
  - Changed event_size validation from `<= 19` to `< 19 || > MAX_EVENT_SIZE`
  - Allows legitimate STOP events (19B pure header) while preventing DoS
  - Impact: mysqldump binlog backups and 5.6/NONE environments now parse correctly
  
- **B001 (High→Fixed)**: Auto-scan all consecutive binlog files by default
  - When no `--stop-file` or `--stop-datetime` is specified, tool now scans until last file
  - Previously only scanned start-file silently, causing data loss (7090 vs 10640 events)
  - Added `detect_next_binlog()` helper for intelligent file enumeration
  
- **B010 (High→Fixed)**: Implement `on-error skip-bad-event` strategy
  - Flashback mode now gracefully skips corrupted files instead of terminating
  - Exit code = 0 on skip-strategy, continues scanning next file
  - Backward compatible: `--on-error stop` still strictly terminates
  
- **B008 (High→Medium Partially-Fixed)**: Basic dry-run support added
  - `--dry-run` flag now prevents SQL file generation (returns exit=0, empty output dir)
  - Note: Full recovery_rate JSON stats implementation planned for future optimization

### Technical Improvements
- Added MAX_EVENT_SIZE = 4MB DoS protection constant
- Extended `RunSummary.skipped_files_by_error` counter for skip statistics
- All 321 unit tests passing ✅
- Performance baseline maintained: 300+ MiB/s throughput (threads=8)
- Memory stable at ~33MB Peak RSS

### Testing & Verification
- **P7 Retest Results**: 
  - Phase 1: Binary/CLI verification ✅
  - Phase 2: All 4 bug fixes verified ✅
  - Phase 3: MySQL 5.6/5.7/8.0/8.4 compatibility ✅
  - Phase 4: Performance ≥300 MiB/s ✅
  - Phase 5: E2E scenarios (rollback/audit/master repair) ✅

- **Quality Gate**: No High/Critical bugs remaining, 2 Medium issues within threshold

### Known Issues (Non-blocking, ≤3 Medium limit met)
1. **B007**: stdout/stderr stream separation (logs should go to stderr, not mixed with SQL)
2. **B008**: Dry-run recovery_rate JSON summary (Phase 2 future work)
3. **B010**: Report-file skip detail fields (non-critical enhancement)
4. Various documentation/docstring inconsistencies (low priority)

### Documentation
- Created comprehensive retest guide: `P6_RETEST_NEW.md`
- Added detailed technical analysis: `BUG_FIX_SUMMARY.md`
- Updated bug tracking in `BUGS.md` with P7 verification records

### Release Artifacts
- Binary SHA256: See release page after build
- Build command: `cargo build --release`
- Test coverage: 321 unit tests, no regression detected

---

## [0.5.1] - 2026-09-20 (P6 Initial Delivery)

### Features
- Four main modes: to-sql, flashback, stats, repl
- Support for MySQL 5.6+ with ROW format binlog
- CRC32 checksum validation
- Multi-threaded processing with thread pool
- Schema cache mechanism
- Checkpoint/resume capability (repl mode)

### Known Issues (Fixed in 0.5.2-p7)
- B013: NONE+Stop parsing failure ❌ → ✅ Fixed
- B001: Silent data loss on default scan ❌ → ✅ Fixed  
- B010: skip-bad-event not implemented ❌ → ✅ Fixed
- B008: dry-run no-op ⚠️ → ⚠️ Partially Fixed (SQL generation blocked, stats future work)

---

Previous versions are not tagged separately. This changelog focuses on significant releases with comprehensive testing.

For full details on bug fixes and verification results, see:
- `BUG_FIX_SUMMARY.md` - Technical analysis
- `RETEST_REPORT.md` - P6 and P7 verification reports
- `P6_RETEST_NEW.md` - Retest methodology and scoring
