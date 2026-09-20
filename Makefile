# my2sql-rs —— P1 任务绑定入口（Task 15 起；其余目标直通 cargo）
.PHONY: test lint fmt difftest

test:
	cargo test

lint:
	cargo clippy --all-targets -- -D warnings

fmt:
	cargo fmt --check

## Task 15: golden 差分（docker mysql + 全类型矩阵 + Go 裁判 vs Rust + 语义比较器）
difftest:
	bash tools/run-difftest.sh
