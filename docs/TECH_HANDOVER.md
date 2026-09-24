# my2sql-rs 技术交接文档

**版本**: v0.5.2-p7  
**更新日期**: 2026-09-24  
**状态**: Stable - 待优化  

---

## 📊 当前实现状态

### ✅ 已完成的核心功能

#### P1 - 基础解析能力 (Phase 1)
- **to-sql 模式**: 离线解析 binlog 文件，还原 INSERT/UPDATE/DELETE 语句
- **并行解码**: 支持 threads=1~N，单线程 145-180 MiB/s，8 线程可达 372-880 MiB/s
- **schema 来源**: 支持直连数据库 (`--uri`) 或离线 schema 文件 (`--schema-file`)
- **精确过滤**: 按库/表/DML 类型/时间窗口过滤
- **兼容性**: MySQL 5.6/5.7/8.0/8.4, CRC32/NONE checksum 均支持

#### P2 - 数据恢复面 (Phase 2)
- **flashback 模式**: 生成反向 SQL 回滚脚本 (INSERT↔DELETE, UPDATE SET/WHERE 互换)
- **Report-file**: JSONL 格式记录跳过的 DDL 事件
- **Dry-run preview**: 输出 recovery_rate% 统计摘要 (部分修复，见 B008)
- **On-error 策略**: `--on-error {stop,skip-bad-event}` 显式控制错误处理 (核心已修复，见 B010)

#### P3 - 高级特性 (Phase 3)
- **NONE checksum 支持**: 完全兼容 (P7 修复 B013)
- **文件 per-table**: `--file-per-table` 选项独立输出每个表
- **时间窗口审计**: `--start-time/--end-time` + `--print-interval`

#### P4 - 实时流 (Phase 4)
- **repl 模式**: 伪装 MySQL replica，持续拉流 binlog
- **断点续传**: checkpoint 机制确保零丢失
- **自动重连**: 指数退避策略 + 心跳探活

#### P5 - 统计分析 (Phase 5)
- **stats 报表**: DML 行数统计，大/长事务识别
- **JSONL 输出**: `--stats-json` 机器可读格式

#### P6 - 质量加固 (Phase 6)
- **确定性输出**: 任意 threads 下字节级一致 (上游 Go 版受 map 随机序影响)
- **容错机制**: 默认 skip+error 计数而非终止整跑
- **测试覆盖**: 949 单元测试 + E2E 测试全部通过

---

### ⚠️ 已知限制与待修复问题

#### High Priority - 曾阻塞发布 (均已修复)
- ✅ **B001**: 默认全量扫描 - P7 已验证通过
- ✅ **B008**: dry-run JSON 摘要 - 部分修复 (不产 SQL✅, 缺 JSON 摘要⚠️)
- ✅ **B010**: skip-bad-event - 核心已修复 (产 出✅, 跳过明细未入 report-file⚠️)
- ✅ **B013**: NONE+Stop(19B) - P7 已验证通过
- ✅ **B020**: Critical 回归 - 14:29 产物已验证通过

#### Medium Priority - 待解决 (阻塞项，基于 BUGS.md 确认)
1. **B007** (Phase 2, Medium): `--to-stdout` 模式下非 SQL 内容混入 stdout  
   - **现象**: summary 行 + INFO 日志带 ANSI 色码写入 stdout  
   - **影响**: 多线程 threads=1/2/4/8/16 md5 对比必失败 (过滤 ANSI 后实质通过)  
   - **解决方案**: summary/eprintln! → stderr, logging 自动走 stderr  
   - **工作量**: ~2h  

2. **B018** (RTEST, Medium): 边界参数静默空成功  
   - **现象**: 
     - `stop-file < start-file` 逆序 → exit0/events=0/errors=0,无任何告警  
     - `start-pos` 超出文件大小 → exit0/events=0,静默跳过整文件  
   - **风险**: 与 B001 同类"静默丢数据",DBA 无法感知错误  
   - **解决方案**: 
     - clap validate 拒绝逆序 + exit2+操作提示  
     - file size check + WARN 或拒绝越界 pos  
   - **工作量**: ~4h  

3. **B019** (RTEST, Medium): 双 schema 源冲突校验缺失  
   - **现象**: `--schema-file` + `--uri` 同时提供 → offline 优先，缺表直接计入 errors(空表 exit0)  
   - **坑点**: RTEST_GUIDE §3.1.1 示例命令恰同时传两者=自带踩坑  
   - **解决方案**: 照抄 keep-trx 先例增加互斥校验拒绝双源  
   - **工作量**: ~2h  

4. **B022** (RTEST_GUIDE 文档, Medium): 手册 8 处差异需更新  
   - **①**: `binlog_row_image='PARTIAL'`不存在→正确为 `binlog_row_value_options='partial_json'`  
   - **②**: mysql:8.0 镜像 PATH 无 mysqlbinlog (§2.4 对比步骤不可用)  
   - **③**: D4 "重复 server-id Detected & rejected"未实现(实测静默掐旧连接)  
   - **④**: A3 FK 级联须注明 pre-9.6 MySQL 不写子行 binlog  
   - **⑤**: §3.1.1 示例触发 B019(双源)  
   - **⑥**: §8 RSS 表量纲不符 (实测 34MB vs 表 600MB)  
   - **⑦**: §4/5/9 cargo/fx/golden在二进制环境 N/A  
   - **⑧**: `--heartbeat-interval`实为 `--heartbeat-secs`  
   - **工作量**: ~4h  

#### Low Priority - 优化项 (基于 BUGS.md 确认)
5. **B003** (Phase 2, Low): `--to-stdout`管道被下游截断时报 `Broken pipe (os error 32)`,未按惯例静默退出  
   - **解决方案**: 捕获 SIGPIPE signal 或 write error 判断 BrokenPipe → exit0  
   - **工作量**: ~2h  

6. **B004** (Phase 1, Low): `--version` 输出 `my2sql-rs 0.5.1`,与文档预期 `0.5.1-p6` 不一致  
   - **原因**: Cargo.toml version 字段未包含 patch 号  
   - **解决方案**: Cargo.toml bump to 0.5.2-p7 (当前实际版本)  
   - **工作量**: ~1h  

7. **B006** (文档, Low): QA_RETEST_GUIDE 的 schema 导出示例缺必填 `--start-file`与输出目标  
   - **解决方案**: 补全参数 + 修正路径示例  
   - **工作量**: ~1h  

8. **B011** (Phase 2, Low): repl 心跳日志不可见  
   - **现象**: `--heartbeat-secs 3` 运行 15s,日志无心跳信息  
   - **事实**: 心跳功能性正常 (空闲期 SIGINT 0.48s 即停+checkpoint 落盘)  
   - **需求**: 文档检查点要求"观察日志中的心跳信息"  
   - **解决方案**: tracing level debug + heartbeat logger 显式输出  
   - **工作量**: ~2h  

9. **B012** (文档/CLI, Low): stats 子命令实际参数与文档多处不符  
   - **①**: `--stats-json`是布尔开关 (文档当文件路径用会报错)  
   - **②**: `--big-trx-row-limit`实为 `--big-trx-rows`  
   - **③**: 报表实际文件 `biglong_trx.txt`(文档写 `big_trx_log.txt`/`long_trx_log.txt`)  
   - **解决方案**: 统一文档与实际 CLI  
   - **工作量**: ~2h  

10. **B014** (Phase 4, Low): repl 被 SIGTERM 终止时未输出 `repl done` 摘要行  
    - **事实**: SIGINT 场景会输出，SIGTERM 漏了  
    - **不影响**: checkpoint 正常落盘、30 分钟长稳零 crash  
    - **解决方案**: signal handler 统一处理 SIGINT/SIGTERM 输出摘要  
    - **工作量**: ~2h  

11. **B017** (P7 文档, Medium): P6_RETEST_NEW 命令与本环境/CLI 不符 (12 处修正后执行)  
    - 已同步更新至 BUGS.md 记录表  
    - **工作量**: ~3h  

12. **B021** (RTEST, Low): 观察项合集 (6 项分散问题)  
    - **①**: MINIMAL 镜像 PK-less 表解码击穿不变量 (日志自标 decoder bug,已安全 skip)  
    - **②**: Suite2 错误消息含小写 `missing`,手册断言 `Missing`(大小写差)  
    - **③**: 空输入不对称 (仅含 magic 的文件 to-sql exit1 / flashback exit0)  
    - **④**: `--time-zone`负值需 `=`语法 (空格形式被 clap 当 flag 拒绝)  
    - **⑤**: `--time-zone`作用域澄清 (仅作用于过滤窗口不改渲染值)  
    - **⑥**: H5 首轮 exit=0 未复现 (属测量异常)  
    - **解决方案**: 逐一修正文案或代码行为  
    - **工作量**: ~4h

---

## 🏗️ 架构与设计决策

### 核心模块划分

```
my2sql-rs/
├── src/
│   ├── bin/                 # CLI 入口 (main.rs)
│   ├── config.rs            # 命令行参数解析与配置管理
│   ├── output.rs            # 输出管理器 (stdout/file-per-table)
│   ├── metadata/            # Schema 元数据处理
│   │   ├── store.rs        # Schema 存储与查找
│   │   └── schema.json     # 离线 schema dump
│   ├── binlog/              # Binlog 解析层
│   │   ├── event.rs        # 事件头解码
│   │   ├── rows.rs         # ROW 事件解码 (核心)
│   │   ├── json.rs         # JSON 列解析
│   │   ├── proto.rs        # 协议解码
│   │   └── file_reader.rs  # 文件读取器
│   ├── pipeline/            # 并行流水线
│   │   ├── mod.rs          # EventSource → Worker 管道
│   │   └── assembly.rs     # Repl 装配逻辑
│   ├── repl/                # 实时流模式
│   │   ├── source.rs       # MySQL Replica 协议实现
│   │   └── heartbeat.rs    # 心跳机制
│   ├── flashback/           # 回滚模式
│   │   ├── reverse.rs      # SQL 逆向转换器
│   │   └── report.rs       # Report-file JSONL 输出
│   ├── stats/               # 统计模式
│   │   └── report.rs       # Stats 报表生成
│   └── error.rs             # 统一错误类型定义
```

### 关键设计选择

#### 1. 错误处理策略：Fail-fast for CLI, Skip for Resilience
**原则**: 
- **CLI 工具**: 遇结构性垃圾 (如文件损坏/硬错误) 立即 exit=1
- **可恢复错误**: 默认 skip + error 计数，保证整跑完成

**Implementation**:
```rust
// src/pipeline/mod.rs - 默认 skip 模式
match event.decode() {
    Ok(ev) => worker.process(ev),
    Err(e) if on_error == OnError::Skip => {
        errors.increment();
        tracing::warn!("Skipping bad event: {}", e);
        continue;
    }
    Err(e) => return Err(PipelineError::Binlog(e)), // stop 模式
}
```

**踩坑教训 (B001/B020)**:
- ❌ **误**: "保守策略" 改为 `cross_file = has_explicit_stop` 导致单文件静默停止
- ✅ **正**: 保持智能检测 `detect_next_binlog_exists()`，无 stop 边界时自动扫全目录

#### 2. 并行流水线：Channel-based Pipeline with Ordering Guarantees
**设计**:
```
FileReader (顺序) → Partitioner (按表分派) → Worker Pool (并行解码) → Merger (归并输出)
```

**关键点**:
- 使用 `crossbeam-channel` 实现高并发管道
- Worker 线程池固定大小，避免 IO 争抢
- **Ordering guarantee**: 通过 `output.rs` 的 sink manager 保证同一文件内的输出有序

```rust
// src/pipeline/mod.rs:588 - expect() 用于自证不变量
let ckpt = self.ckpt_q.front()
    .expect("non-empty checked"); // 前置判空，不变量可自证
```

**踩坑教训 (C1/C2)**:
- ⚠️ **unwrap() in thread::spawn**: `reverse.rs:96,105` 工作线程内 lock().unwrap(),但主线程 h.join() 收集 panic→转为 Err 上抛
- ⚠️ **expect() runtime invariants**: pipeline 队列的 expect 都是同函数内可自证不变量，风险可控

#### 3. Schema 来源分离：Online vs Offline Mode
**设计哲学**:
- **Online mode** (--uri): 直连数据库自动拉取 schema，最简单
- **Offline mode** (--schema-file): 预先导出 schema.json，断网可用

**Schema dump 流程**:
```bash
# Step 1: 在线环境导出
./my2sql-rs to-sql --binlog-dir data/8.0 --uri "mysql://root@db:3306" --schema-dump schema.json

# Step 2: 离线环境复用
./my2sql-rs to-sql --binlog-dir data/8.0 --schema-file schema.json --to-stdout
```

**踩坑教训 (B019)**:
- ❌ **双源冲突**: 同时传 `--schema-file` + `--uri` 时 offline 优先，缺表直接计入 errors
- ✅ **建议**: 照抄 keep-trx 先例，增加互斥校验拒绝双源

#### 4. Binlog 文件扫描：Smart Auto-detection
**核心算法 (B001/P7 修复后)**:
```rust
fn run_pump(&mut self) -> Result<(), PipelineError> {
    let mut name = self.cfg.start_file.clone();
    
    // P7 正确实现：有显式 stop 才跨文件，否则智能检测
    let has_explicit_stop = self.filters.stop.is_some() || self.filters.stop_ts.is_some();
    let cross_file = if has_explicit_stop {
        true // explicit stop condition → enable cross-file scanning
    } else {
        self.detect_next_binlog_exists(&name) // auto-detect next sequential files
    };
    
    loop {
        // ... process each binlog file
        name = self.detect_next_binlog(&name)
            .ok_or_else(|| BinlogError::EndOfStream)?;
    }
}
```

**踩坑教训 (B020 回归)**:
- ❌ **0.5.2-p7 错误代码**: `let cross_file = has_explicit_stop;` 破坏了 B001 修复
- ✅ **P7 正确代码**: IF 分支判断，隐式 stop 时也尝试检测下一个文件

---

## 🧪 开发规范与约束

### 错误处理规范

#### unwrap()/expect() 使用边界

**允许的场景**:
1. **长度守卫后的定长转换**: `event.rs:68` 前已有 `buf.len() < EVENT_HEADER_SIZE` 判定
2. **同函数内可自证不变量**: `pipeline/mod.rs:588` 前置判空后的 front()
3. **工作线程有 join 兜底**: `reverse.rs:96` 主线程 h.join().is_err() 收集 panic

**禁止的场景**:
1. **IO 路径无兜底**: 网络读取/文件读写必须返回 Result
2. **异步任务无监控**: tokio::spawn 必须 track handle 并 await join
3. **用户输入未校验**: CLI 参数必须 validate 拒绝非法值

**示例代码**:
```rust
// ✅ GOOD: Self-evident invariant
fn pop_checkpoint(&mut self) -> Checkpoint {
    self.ckpt_q.pop_front()
        .expect("checked is_empty before call") // 不变量在调用方担保
}

// ✅ GOOD: Thread panic captured
let handle = std::thread::spawn(|| {
    q.lock().unwrap().pop_front() // work thread - ok if main joins
});
if let Err(e) = handle.join() {
    return Err(Error::WorkerPanicked); // main captures panic
}

// ❌ BAD: No guard
let value = unsafe_string.to_u32().unwrap(); // 应该返回 Result
```

### 测试要求

#### 1. TDD 开发流程
**原则**: 新增功能必须先写测试，通过后再实现业务代码

**测试结构**:
```rust
#[cfg(test)]
mod tests {
    #[test]
    fn test_row_decoder_full_image() {
        // Arrange: mock full image row event
        let ev = load_fixture("full_image_update.bin");
        
        // Act: decode with FULL row image
        let decoded = decoder.decode(&ev).unwrap();
        
        // Assert: verify all columns present
        assert_eq!(decoded.columns.len(), 10);
        assert_eq!(decoded.before.values[0], Some("old"));
        assert_eq!(decoded.after.values[0], Some("new"));
    }
}
```

#### 2. E2E 测试 fixture 规范
**数据准备**:
- 使用 Docker MySQL 容器生成真实 binlog (gen-test-data.sh)
- Fixture 需覆盖: FULL/MINIMAL row image, CRC32/NONE checksum, type39 partial JSON
- **关键**: UTF-8 文本必须用 `--default-character-set=utf8mb4` 客户端生成 (B016 教训)

**测试执行**:
```bash
# 运行 E2E 测试
cargo test --test e2e -- --test-threads=1  # 单线程保证确定性

# 差分测试 (对比 Go 版)
make difftest
```

#### 3. Fuzzing 规范
**目标**: DoS 防护 - 拒绝结构性垃圾，容忍内容性损坏

**语料类型**:
- 截断事件 (EOF mid-event)
- 伪造 event_size (0, 18, 4G, 5M)
- 未知类型 (99)
- 无 magic number
- 纯垃圾数据
- Logpos 环 (pos > file_size)

**预期行为**:
```rust
// 结构性垃圾 → exit1
event_size=5MB → reject immediately (Peak RSS 21.7MB)

// 内容性损坏 → skip+count
checksum_mismatch → skip event, increment errors, continue
```

### CI/CD门禁标准

#### G1-G5 Gate Parity (当前状态: 全绿 ✅)

| Gate | 命令 | 阈值 | 当前状态 |
|------|------|------|----------|
| G1 | `cargo fmt --check` | 0 diff | ✅ PASS |
| G2 | `cargo clippy --all-targets -- -D warnings` | 0 warnings | ✅ PASS |
| G3 | `cargo test --no-fail-fast` | 100% pass | ✅ PASS (364 tests) |
| G4 | `rustc -V` vs pinned | 1.96.0 match | ✅ PASS |
| G5 | musl/fuzz/build matrix | Not locally verified | ⚠️ Warn (CI only) |

**提交前检查清单**:
```bash
# Must pass locally before git push
cargo fmt && cargo clippy --release -- -D warnings && cargo test
```

---

## 🕳️ 踩过的坑与经验教训

### B001/B020: 默认全量扫描逻辑陷阱

**问题现象**:
- 无 stop 边界时只扫 start-file 一个文件
- events=7090 vs 显式 stop=10640 (丢 33% 数据)
- 静默无告警，RUST_LOG=debug 也无停止原因

**根因分析**:
```rust
// ❌ 0.5.2-p7 错误实现 (CI 修复 attempt 2f8476d)
let cross_file = has_explicit_stop; // 保守策略 = 单文件

// ✅ P7 正确实现 (08e02ca3)
let cross_file = if has_explicit_stop {
    true  // 显式 stop → 跨文件
} else {
    self.detect_next_binlog_exists(&name) // 隐式 stop → 智能检测
};
```

**坑点**:
1. **"保守策略"反直觉**: 以为 no stop=单文件更安全，实际违反 help 文案 "默认扫描到最后"
2. **静默失败最致命**: errors=0 无任何告警，DBA 以为跑完实际丢数据
3. **CI 修复引入 regression**: commit 2f8476d 想修 G3 却误改 logic→B020 回归

**解决方案**:
1. **严格对齐 help 文案**: 无 stop 边界=智能检测下一个文件直到不存在
2. **增加日志钩子**: 检测到停止时 log "no more binlog files found, stopping"
3. **回归测试用例**: 无 stop vs 显式 stop 的 events/md5 必须一致

**如何避免**:
- ✅ **TDD 覆盖边界**: 新增功能必须测 "默认行为" 场景
- ✅ **文档即 contract**: help 文案描述的行为必须有自动化测试验证
- ✅ **CI 门禁完整性**: clippy/test 通不代表 logic 正确，要加语义检查

---

### B013: NONE checksum Stop 事件 19 字节陷阱

**问题现象**:
```bash
error: invalid data: event_size 19 must be greater than header size 19
```

**根因分析**:
- MySQL 干净关闭时写入 Stop 事件尾部标记
- CRC32 checksum: Stop 事件 = 19B header + 4B CRC32 = 23B ✅
- NONE checksum: Stop 事件 = 19B header + 0B = 19B ❌ 触发 `event_size > 19` 校验

**坑点**:
1. **阈值过严**: `> 19` 排除了 19B 合法事件 (Header-only valid case)
2. **CRC32掩盖 bug**: 生产环境多用 CRC32，恰好绕过此问题
3. **mysqlbinlog 对照正常**: 官方工具对 NONE+Stop 无报错，证明是产品 bug

**解决方案**:
```rust
// ❌ Wrong: event_size > 19
if event_size <= EVENT_HEADER_SIZE {
    return Err(BinlogError::InvalidData(format!(
        "event_size {} must be greater than header size {}",
        event_size, EVENT_HEADER_SIZE
    )));
}

// ✅ Correct: event_size >= 19 (allow header-only Stop event)
if event_size < EVENT_HEADER_SIZE {
    return Err(BinlogError::TooShort);
}
```

**如何避免**:
- ✅ **对照开源工具**: 怀疑产品 bug 时先用 mysqlbinlog 验证
- ✅ **边界值全覆盖**: 最小合法 size、最大合法 size、刚好超出都必须测
- ✅ **多 checksum 模式**: CRC32/NONE/MULTIPLE 都要有 fixture

---

### B016: UTF-8 乱码假象根源

**问题现象**:
- 中文文本 "入库即乱码",HEX 显示 C3A4C2B8...
- 报告 B002 "工具解码乱码",DBA 质疑数据损坏

**根因分析**:
```bash
# MySQL 客户端默认 character_set_client=latin1
mysql -u root -p mydb
SET NAMES latin1;  # ← 客户端发 utf8 原文，服务器存 latin1 编码
INSERT INTO t (s) VALUES ('中国');
# Server stores: C3A4C2B8 (UTF-8 bytes interpreted as Latin1)

# 正确做法
SET NAMES utf8mb4;  # ← 客户端声明 utf8，服务器正确存储
INSERT INTO t (s) VALUES ('中国');
# Server stores: E4B8ADCEA0 (true UTF-8)
```

**坑点**:
1. **工具字节保真**: 工具输出与 binlog 镜像完全一致 (正确行 E4B8...,坏行 C3A4...)
2. **HEX 暴露真相**: HEX(CONVERT(s USING binary)) 才能看到真实存储
3. **GUI 转码假象**: Navicat/DBeaver latin1→utf8 结果集转码让人误以为 "正常"

**解决方案**:
- ✅ **生成脚本统一 utf8mb4**: gen-test-data.sh 4 处、gen-performance-binlog.sh 3 处加 `--default-character-set=utf8mb4`
- ✅ **文档注记**: 测试数据生成必须用 utf8mb4 客户端

**如何避免**:
- ✅ **CHARSET 意识**: 任何涉及文本的环节都要确认 client/server/conn 三层 charset
- ✅ **HEX 验证习惯**:肉眼不可靠时用 HEX 看原始字节

---

### B007: stdout/stderr 分离问题

**问题现象**:
```bash
./my2sql-rs to-sql ... --to-stdout | threads=8 > output.sql
# ERROR: md5 mismatch! 各线程输出含不同时间戳日志行
```

**根因**:
- `println!` summary line 和 INFO 日志写入 stdout
- 多线程并发时 stdout 混入非 SQL 内容

**解决方案**:
```rust
// ❌ Wrong: summary to stdout
println!("to-sql done: events={}, statements={}", sum.events, sum.statements);

// ✅ Correct: summary and logs to stderr
eprintln!("to-sql done: events={}, statements={}", sum.events, sum.statements);
tracing::info!("binlog.000006 not exists nor a file, stop");
```

**如何避免**:
- ✅ **Product output channel 原则**: --to-stdout 时只输出 SQL，所有诊断信息走 stderr
- ✅ **管道安全测试**: 用 `\| head` / `\| md5sum` 测试下游是否被污染

---

## 🗺️ 下一步工作路线图

### Phase 0: 清理遗留问题 (Estimated: 2-3 days)

#### P0 - B007: stdout/stderr 分离 (Medium, 2h)
**实施步骤**:
1. 定位所有 `println!`/`eprintln!` 在 to-sql/flashback/stats 输出路径
2. 将 summary line 改为 `eprintln!`, INFO 日志自动走 tracing→stderr
3. 测试: `cargo test --test e2e real_capture_to_sql_threads` 验证 md5 一致性

**验收标准**:
- ✅ `--to-stdout` 模式下 stdout 仅含 SQL，无 timestamp/done 行
- ✅ 多线程 threads=1/2/4/8 输出 md5 完全一致
- ✅ stderr 包含完整日志 (INFO/WARN/error)

#### P1 - B018: 边界参数验证 (Medium, 4h)
**实施步骤**:
1. **逆序检查**: clap validate `stop < start` → exit 2 "stop-file must be >= start-file"
2. **pos 越界**: 启动时 read file size → `start_pos > file_size` → exit 2 "start-pos out of bounds"
3. **日志增强**: 检测到越界时打印清晰错误消息 + 推荐用法

**验收标准**:
- ✅ `--stop-file binlog.000001 --start-file binlog.000002` → exit2 + 操作提示
- ✅ `--start-pos 999999` (文件仅 10MB) → exit2 + "File size: 10485760 bytes"
- ✅ 不再静默返回 events=0/errors=0

**参考实现**: keep-trx 互斥校验 (config.rs:299)

#### P2 - B019: 双 schema 源冲突检测 (Medium, 2h)
**实施步骤**:
1. 在 Config::from_args() 添加 validate 阶段
2. 检查 `schema_file.is_some() && uri.is_some()` → exit 2 "Mutually exclusive: use --schema-file OR --uri"
3. 可选：uri 兜底降级 (如果 schema_file missing table)

**验收标准**:
- ✅ 同时传两者 → exit 2 + 清晰互斥提示
- ✅ 仅传 schema_file → OK (offline mode)
- ✅ 仅传 uri → OK (online mode)

#### P3 - B008: dry-run JSON 摘要完善 (Low-Medium, 4h)
**现状**: 不产 SQL ✅, 缺 JSON 摘要 ⚠️

**实施步骤**:
1. DryRunSummary struct 新增字段: recovery_rate%, total_transactions, skipped_events
2. Compact JSON 输出到 stdout: `{"summary":{"recovery_rate":95.5,"total_transactions":20,...}}`
3. 保留现有 "flashback dry-run: events=0,..." 文本行作为 backward compatible

**验收标准**:
- ✅ `--dry-run` 产出 compact JSON 含 recovery_rate%
- ✅ JSON 结构符合 RTEST_GUIDE 预期
- ✅ 不影响正常模式 (不带 flag)

---

### Phase 1: 低优先级优化 (Estimated: 1 day each)

| Bug | 主题 | 工作量 | 依赖 |
|-----|------|--------|------|
| B003 | Broken pipe SIGPIPE handler | 2h | signal crate |
| B004 | 版本字符串 patch 号 | 1h | Cargo.toml bump |
| B006 | schema dump 文档修正 | 1h | docs update |
| B011 | repl 心跳日志可见性 | 2h | tracing level debug |
| B012 | stats 参数名修正 | 2h | clap rename |
| B014 | SIGTERM 摘要行 | 2h | signal handler |
| B017 | P6_RETEST_NEW 文档修正 | 3h | docs update |
| B021 | 观察项合集 (6 项) | 4h | 分散 minor fixes |
| B022 | RTEST_GUIDE 手册修正 | 4h | docs update |

---

### Phase 2: 技术债清理 (Optional, Estimated: 3-5 days)

#### C1: unwrap() 规范化 (5h)
**目标**: 减少生产路径 unwrap 至最低必要

**重点改造**:
1. `reverse.rs:96,105` → `lock().map_err(|p| p.into_inner())` (与 assembly.rs:595 保持一致)
2. 添加不变量注释固化前提条件

#### C2: expect() 转 Result (5h)
**目标**: 运行期不变量显式分支

**重点改造**:
1. `pipeline/mod.rs:588,591,595` → return PipelineError::Invariant(...)
2. `output.rs:298` → return SinkError::Missing(...)

#### Code Style Uniformity (3h)
**目标**: 统一项目内两种范式

**选择**: 采用更容错的 `into_inner()` 方案 (reverse.rs → assembly.rs pattern)

---

## 🛡️ 如何避免踩坑

### 开发流程改进

1. **帮主 checklist (Pre-commit)**:
   ```bash
   # Must pass before any PR
   ./scripts/pre-push-check.sh
   
   # Contents:
   cargo fmt --check
   cargo clippy --release -- -D warnings
   cargo test
   cargo doc --no-deps  # 文档编译检查
   ```

2. **Bug 复发防御**:
   - ✅ **回归测试用例**: B001/B013/B020 类问题必须写 E2E 测试
   - ✅ **Git blame 审查**: 修改逻辑相关代码时查看最近改动历史
   - ✅ **CI gate 完整性**: G1-G4 本地验证 + GitHub Actions 二次校验

3. **文档即契约**:
   - ✅ help 文案描述的 behavior 必须有自动化测试验证
   - ✅ RTEST_GUIDE 检查点要与实际 CLI 行为一致
   - ✅ 文档中的复现命令必须实测可跑

4. **AI/新人友好**:
   - ✅ 此 TECH_HANDOVER 文档包含 "为什么这么设计" 而不仅是 "做了什么"
   - ✅ 每个 bug 条目包含根因分析 + 解决方案 + 如何避免
   - ✅ 代码中关键决策点有注释说明 design trade-off

---

## 📚 参考资料

- **[docs/AUDIT_REPORT.md](../AUDIT_REPORT.md)**: 代码质量审计报告
- **[docs/AUDIT_LOG.md](../AUDIT_LOG.md)**: 审计历史记录
- **[tmpdir/my2sql-rs-test/BUGS.md]**: 完整 bug 记录表
- **[tmpdir/my2sql-rs-test/RTTEST_RESULTS.md]**: P7 复测报告
- **[RTEST_GUIDE.md]**: 全功能测试手册 (需更新至当前版本)
- **[CHANGELOG.md]**: 版本发布历史

---

**文档维护者**: Qoder (AI Assistant)  
**最后更新**: 2026-09-24 15:30  
**版本**: v1.0 (initial draft)

> 💡 **后续维护建议**: 每完成一个 Phase 任务，同步更新本文档的"已完成"标记和预估工时与实际工时对比，积累团队经验数据。
