# my2sql-rs 常见使用案例

本文档收集了 MySQL binlog 解析工具的常见使用场景和解决方案，涵盖数据恢复、审计分析、主从同步等实际业务需求。

---

## 📋 目录

1. [数据恢复](#1-数据恢复)
2. [审计分析](#2-审计分析)
3. [主从同步修复](#3-主从同步修复)
4. [性能监控](#4-性能监控)
5. [架构演进](#5-架构演进)
6. [离线分析](#6-离线分析)
7. [实时监控](#7-实时监控)
8. [高级技巧](#8-高级技巧)

---

## 1. 数据恢复

### 场景 1.1：误删除数据恢复 🚑

**问题**：刚执行 `DELETE FROM users WHERE id = 5;`，发现删错了！

**解决方案**：

```bash
# Step 1: 找到删除操作发生的 binlog 文件
mysql -uroot -p -e "SHOW BINARY LOGS;"
# 输出：mysql-bin.000100  123456
#       mysql-bin.000101  789012  ← 删除发生在这个文件

# Step 2: 生成回滚 SQL（flashback）
./my2sql-rs flashback \
  --binlog-dir ./binlogs \
  --start-file mysql-bin.000101 \
  --uri "mysql://root@127.0.0.1:3306" \
  --threads 8 \
  --output-dir ./recovery

# Step 3: 预览要恢复的数据（dry-run 模式）
./my2sql-rs flashback \
  --binlog-dir ./binlogs \
  --start-file mysql-bin.000101 \
  --uri "mysql://root@127.0.0.1:3306" \
  --dry-run

# 输出：{"summary":{"recovery_rate":95.5,"total_transactions":20,"skipped_events":1},...}

# Step 4: 检查恢复的 SQL 文件
cat ./recovery/flashback.1.sql
# 包含：INSERT INTO users (...) VALUES (...);  ← 被删的那条记录

# Step 5: 执行恢复
mysql -uroot -p mydb < ./recovery/flashback.1.sql
```

**关键点**：
- ✅ 先 dry-run 确认能恢复多少
- ✅ 检查跳过事件报告（report-file）
- ✅ 在小事务后尽快执行，避免新操作覆盖

---

### 场景 1.2：表结构变更导致的数据丢失

**问题**：误执行 `ALTER TABLE` 修改了字段类型，导致数据异常。

**解决方案**：

```bash
# 识别 ALTER TABLE 的位置
./my2sql-rs to-sql \
  --binlog-dir ./binlogs \
  --start-file mysql-bin.000100 \
  --dml query \
  --to-stdout | grep -i "alter table\|create table"

# 假设 ALTER TABLE 在 position 50000
# 生成 Alter 之前的完整数据快照
./my2sql-rs to-sql \
  --binlog-dir ./binlogs \
  --start-file mysql-bin.000100 \
  --end-pos 50000 \
  --to-stdout > before_alter.sql

# 如果已有当前备份，直接恢复到 alter 前的状态
mysql -uroot -p mydb < before_alter.sql
```

---

### 场景 1.3：灾难性数据损坏恢复

**问题**：数据库崩溃，binlog 还在，但部分数据损坏。

**解决方案**：

```bash
# Step 1: 尝试从最近备份恢复
mysqldump --single-transaction mydb | mysql -uroot -p mydb_restored

# Step 2: 找出备份后的所有 DML
./my2sql-rs to-sql \
  --binlog-dir ./binlogs \
  --start-file mysql-bin.000101 \
  --end-file mysql-bin.000110 \
  --to-stdout > post_backup_dml.sql

# Step 3: 应用到恢复的库
mysql -uroot -p mydb_restored < post_backup_dml.sql
```

---

## 2. 审计分析

### 场景 2.1：时间段操作审计 📝

**问题**：想知道昨天下午 2 点到 4 点之间对订单做了什么修改？

**解决方案**：

```bash
# 提取特定时间窗口的 DML
./my2sql-rs to-sql \
  --binlog-dir ./binlogs \
  --start-time "2026-09-22 14:00:00" \
  --end-time "2026-09-22 16:00:00" \
  --db orders \
  --table order_items \
  --dml update,delete \
  --add-extra-info \
  --to-stdout > audit_report.sql

# 查看审计报告
less audit_report.sql

# 统计报表（行数汇总）
./my2sql-rs stats \
  --binlog-dir ./binlogs \
  --start-time "2026-09-22 14:00:00" \
  --end-time "2026-09-22 16:00:00" \
  --output-dir ./stats_report

# 查看结果
cat ./stats_report/binlog_status.txt
# 格式：文件名  开始时间  结束时间  DML 行数  Update 数  Delete 数  Insert 数  大事务 长事务 库名  表名
```

---

### 场景 2.2：用户行为追踪

**问题**：某个用户频繁修改数据，需要审计其操作历史。

**解决方案**：

```bash
# 假设用户 ID 范围是 10000-20000
./my2sql-rs to-sql \
  --binlog-dir ./binlogs \
  --db users \
  --dml update,delete \
  --to-stdout | grep -E "WHERE.*id.*=[[:space:]]*(1[0-9]{4}|20000)" > user_activity.sql

# 统计某用户的操作次数
grep -c "UPDATE" user_activity.sql  # Update 次数
grep -c "DELETE" user_activity.sql  # Delete 次数
```

---

### 场景 2.3：合规性审计

**问题**：满足 GDPR/HIPAA 等合规要求，需要记录所有敏感数据访问。

**解决方案**：

```bash
# 定期导出审计日志
#!/bin/bash
# cron: 每天凌晨 2 点
DATE=$(date +%Y%m%d)
START_FILE=$(mysql -uroot -N -e "SHOW BINARY LOGS" | awk 'NR==2 {print $1}')

./my2sql-rs to-sql \
  --binlog-dir /var/lib/mysql \
  --start-file "$START_FILE" \
  --uri "mysql://audit_user:@localhost:3306" \
  --time-zone +00:00 \
  --file-per-table \
  --output-dir "/audit/$DATE"

# 加密归档
tar czf "/audit/archive_$DATE.tar.gz" "/audit/$DATE"
rm -rf "/audit/$DATE"
```

---

## 3. 主从同步修复

### 场景 3.1：新 master 数据不一致修复 🔧

**问题**：主从切换后，新 master 缺少某些数据（原 master 宕机时未同步）。

**解决方案**：

```bash
# Step 1: 从旧 master 复制缺失的 binlog 到新环境
scp root@old-master:/var/lib/mysql/mysql-bin.000200* ./binlogs/

# Step 2: 生成差异 SQL（到切换点之前）
./my2sql-rs to-sql \
  --binlog-dir ./binlogs \
  --start-file mysql-bin.000200 \
  --stop-file mysql-bin.000210 \
  --to-stdout > missing_data.sql

# Step 3: 应用到新 master
mysql -uroot -p new-master < missing_data.sql

# Step 4: 验证一致性
mysql -uroot -p new-master -e "SELECT COUNT(*) FROM users;"
mysql -uroot -p old-master-backup -e "SELECT COUNT(*) FROM users;"
```

---

### 场景 3.2：GTID 位置校准

**问题**：主从 GTID 位置不同步，导致复制中断。

**解决方案**：

```bash
# Step 1: 分析 GTID 差异
./my2sql-rs to-sql \
  --binlog-dir ./binlogs \
  --add-extra-info \
  --to-stdout | grep "^#" | head -20

# Step 2: 找到断开的 GTID
# 假设需要在 Slave 上设置：
RESET SLAVE;
SET GLOBAL gtid_purged='xxx-xxx-xxx:1-1000';  # 已执行的 GTID 范围
CHANGE MASTER TO MASTER_LOG_FILE='mysql-bin.000200', MASTER_LOG_POS=12345;
START SLAVE;
```

---

## 4. 性能监控

### 场景 4.1：大事务识别和优化 📊

**问题**：发现主从延迟高，怀疑有大事务影响。

**解决方案**：

```bash
# 找出大事务（≥500 行）和长事务（≥5 分钟）
./my2sql-rs stats \
  --binlog-dir ./binlogs \
  --big-trx-row-limit 500 \
  --long-trx-seconds 300 \
  --output-dir ./trx_analysis

# 查看大事务明细
cat ./trx_analysis/big_trx_log.txt

# 查看长事务明细  
cat ./trx_analysis/long_trx_log.txt

# 识别高频更新表
cat ./trx_analysis/trx_by_table_stats.txt
```

**优化建议**：
- 拆分大事务为小批次
- 添加缺失索引减少锁竞争
- 考虑异步处理非关键更新

---

### 场景 4.2：慢查询源头分析

**问题**：为什么这个表更新这么频繁？

**解决方案**：

```bash
# 统计各表的 DML 分布
./my2sql-rs stats \
  --binlog-dir ./binlogs \
  --output-dir ./dml_stats

# 查看表级别的 DML 热度
sort -k7 -rn ./dml_stats/binlog_status.txt | head -20
# 第 7 列是总 DML 行数，降序排列

# 针对热点表深入分析
./my2sql-rs stats \
  --binlog-dir ./binlogs \
  --db orders \
  --table order_items \
  --output-dir ./orders_detail
```

---

## 5. 架构演进

### 场景 5.1：数据迁移到新表结构 🔄

**问题**：需要将历史数据从旧表结构迁移到新表结构。

**解决方案**：

```bash
# Step 1: 导出所有历史数据的 INSERT 语句
./my2sql-rs to-sql \
  --binlog-dir ./old_logs \
  --db old_schema \
  --table products \
  --start-file mysql-bin.000001 \
  --stop-file mysql-bin.000500 \
  --dml insert \
  --to-stdout > product_inserts.sql

# Step 2: 在新库导入（可先调整字段顺序）
mysql -uroot -p new_schema < product_inserts.sql

# Step 3: 如果需要 UPDATE 现有数据
./my2sql-rs flashback \
  --binlog-dir ./update_logs \
  --uri "mysql://root@new_schema" \
  --output-dir ./updates

# 应用更新
mysql -uroot -p new_schema < ./updates/flashback.1.sql
```

---

### 场景 5.2：跨库合并数据

**问题**：多个分库的数据需要合并到一个中心库。

**解决方案**：

```bash
# 为每个分库导出 binlog SQL
for db in sharding_0 sharding_1 sharding_2; do
  ./my2sql-rs to-sql \
    --binlog-dir /data/${db}/binlogs \
    --uri "mysql://root@${db}:3306" \
    --to-stdout > ${db}_sql.sql
done

# 合并并去重（基于主键）
cat sharding_*.sql | sqlfluff --unique-by-primary-key > merged.sql

# 导入到中心库
mysql -uroot -p center_db < merged.sql
```

---

## 6. 离线分析

### 场景 6.1：开发机测试数据生成 🧪

**问题**：开发机没有生产数据，需要真实数据用于测试。

**解决方案**：

```bash
# Step 1: 从生产环境复制最近的 binlog
scp prod-server:/var/lib/mysql/mysql-bin.001000 ./dev_binlogs/

# Step 2: 导出 schema（在生产环境或临时库）
./my2sql-rs to-sql \
  --uri "mysql://root@prod-db" \
  --schema-dump schema.json

# Step 3: 在开发机解码（无需连接数据库）
./my2sql-rs to-sql \
  --binlog-dir ./dev_binlogs \
  --schema-file schema.json \
  --to-stdout > test_data.sql

# Step 4: 导入到开发库
mysql -uroot -p dev_db < test_data.sql
```

---

### 场景 6.2：安全合规下的数据分析

**问题**：无法直连生产数据库，只能 access binlog 文件。

**解决方案**：

```bash
# Step 1: 提前从可信任环境导出 schema.json
# （网络隔离前最后一次机会）

# Step 2: 将 schema.json + binlog 拷贝到安全沙箱
scp schema.json safe-box:/tmp/
scp binlogs/* safe-box:/tmp/binlogs/

# Step 3: 在沙箱内离线分析
cd safe-box
./my2sql-rs to-sql \
  --binlog-dir ./tmp/binlogs \
  --schema-file ./tmp/schema.json \
  --output-dir ./analysis

# Step 4: 仅导出统计分析结果（不含敏感数据）
cat ./analysis/binlog_status.txt > summary.txt
scp summary.txt user@example.com
```

---

## 7. 实时监控

### 场景 7.1：实时数据管道 👀

**问题**：需要实时监控数据库变更并触发下游动作。

**解决方案**：

```bash
# repl 模式持续拉流（伪装成 MySQL replica）
./my2sql-rs repl \
  --binlog-dir /nonused \
  --start-file "" \
  --uri "mysql://root@127.0.0.1:3306" \
  --server-id 9527 \
  --time-zone +00:00 \
  --output-dir ./realtime \
  --heartbeat-secs 30 \
  --stop-datetime "$(date -u -d '+1 hour' '+%Y-%m-%d %H:%M:%S')" &

# 后台运行
nohup ./my2sql-rs repl ... > /dev/null 2>&1 &

# 实时监控进程
watch -n 5 'ls -lh ./realtime/*.sql | tail -5'

# 查看进度（checkpoint 自动记录）
cat ./realtime/resume.json
```

**下游集成示例**：
```bash
#!/bin/bash
# 监听新生成的 SQL 文件并触发 webhook
inotifywait -m ./realtime --format '%f' -e close_write | while read file; do
  if [[ $file == *.sql ]]; then
    curl -X POST https://api.example.com/webhook \
      -H "Content-Type: application/json" \
      -d "{\"event\":\"binlog_update\",\"file\":\"$file\"}"
  fi
done
```

---

## 8. 高级技巧

### 场景 8.1：增量备份与验证

**问题**：如何验证备份的完整性？

**解决方案**：

```bash
# Step 1: 定期备份 binlog 和 schema
BACKUP_DIR="/backup/binlog_$(date +%Y%m%d)"
mkdir -p "$BACKUP_DIR"

cp /var/lib/mysql/mysql-bin.* "$BACKUP_DIR/"
./my2sql-rs to-sql \
  --uri "mysql://root@prod-db" \
  --schema-dump "$BACKUP_DIR/schema.json"

# Step 2: 验证备份可用性
# 方案 A：使用 Docker 临时实例（快速验证）
docker run -d --name test-mysql mysql:8.0
docker cp "$BACKUP_DIR/schema.json" test-mysql:/tmp/
docker exec test-mysql mysql -uroot -p < <(grep CREATE TABLE /tmp/schema.json)

# 方案 B：使用独立的测试 MySQL 实例（生产环境推荐）
# 连接到您的测试数据库服务器
mysql -h test-server -uroot -p -e "CREATE DATABASE IF NOT EXISTS verify_db;"

./my2sql-rs to-sql \
  --binlog-dir "$BACKUP_DIR" \
  --uri "mysql://root@test-db-server" \
  --to-stdout > verify.sql

mysql -uroot -p verify_db < verify.sql
```

---

### 场景 8.2：自定义过滤规则

**问题**：只需要特定的表和操作类型。

**解决方案**：

```bash
# 只分析核心业务的 UPDATE 操作
./my2sql-rs to-sql \
  --binlog-dir ./binlogs \
  --db core_business \
  --table users,orders,payments \
  --dml update \
  --ignore-table %tmp%,%cache% \
  --output-dir ./filtered

# 排除系统库
./my2sql-rs to-sql \
  --binlog-dir ./binlogs \
  --ignore-db performance_schema,information_schema,sys \
  --add-extra-info \
  --to-stdout
```

---

### 场景 8.3：批量处理多天 binlog

**问题**：需要分析一周的数据。

**解决方案**：

```bash
#!/bin/bash
# 批量处理连续多天的 binlog

START_DATE="2026-09-15"
END_DATE="2026-09-21"

for date in $(seq -d '-' "$START_DATE" "$END_DATE"); do
  BINLOG_FILE="mysql-bin.$(date -d "$date" +%y%m%d%H)"
  
  echo "Processing $BINLOG_FILE..."
  
  ./my2sql-rs to-sql \
    --binlog-dir /var/lib/mysql \
    --start-file "$BINLOG_FILE" \
    --end-file "$BINLOG_FILE" \
    --output-dir "./daily_reports/$date" \
    --stats-json "./daily_reports/$date/events.jsonl"
done

# 合并结果
cat daily_reports/*/binlog_status.txt | sort > weekly_summary.txt
```

---

## 📚 参考资源

- [命令行参数详解](COMMAND_LINE_OPTIONS.md)
- [快速入门指南](../README.md)
- [CHANGELOG 发布历史](../CHANGELOG.md)
- [上游 my2sql-go](https://github.com/liuhr/my2sql)

---

## 💡 贡献案例

欢迎提交新的使用场景！请通过 Issue 或 PR 分享您的独特用法。
