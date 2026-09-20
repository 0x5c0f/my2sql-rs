# my2sql-rs —— P1 任务绑定入口（Task 15 起；其余目标直通 cargo）
.PHONY: test lint fmt difftest compat

test:
	cargo test

lint:
	cargo clippy --all-targets -- -D warnings

fmt:
	cargo fmt --check

## Task 15: golden 差分（docker mysql + 全类型矩阵 + Go 裁判 vs Rust + 语义比较器）
difftest:
	bash tools/run-difftest.sh

## Task 17: 全版本兼容矩阵（5.6/5.7/8.0/8.4 × {差分, checksum 双态, 离线回放, 8.4 caching_sha2}）
compat:
	bash tools/compat-matrix.sh
