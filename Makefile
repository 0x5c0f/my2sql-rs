# my2sql-rs —— P1 任务绑定入口（Task 15 起；其余目标直通 cargo）
.PHONY: test lint fmt difftest compat repl-test

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

## P3 T6: repl live e2e（起唯一名容器/等健康 → MY2SQL_TEST_URI(+CTR) 下跑
## tests/repl.rs 的 --ignored 件 → EXIT trap 清容器）。容器件复用
## tools/repl-e2e-lib.sh（T7 矩阵同 source）。--test-threads=1：live 件共
## 享单容器，位点窗口须串行。VER=8.0 可换版本（T7 矩阵入口同理由）。
repl-test:
	bash -c 'set -euo pipefail; source tools/repl-e2e-lib.sh; \
		trap "p3e2e_container_stop" EXIT INT TERM; \
		p3e2e_container_start "$${VER:-8.0}"; \
		export MY2SQL_TEST_URI="$$P3E2E_URI" MY2SQL_TEST_CTR="$$P3E2E_CTR"; \
		cargo test --test repl -- --ignored --nocapture --test-threads=1'
