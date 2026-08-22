# Logging Guidelines — index

## 零日志原则

- 纯内存数据结构，操作均为 O(小) 位图运算，无需日志。
- 性能观测走 criterion 基准（`benches/budget.rs`），不走日志。
- 若未来引入 tracing，只允许在批量导入边界打 span，热路径（evaluate/tag_counts）禁止日志——D3 预算 100 万条 <1ms，任何格式化开销都超标。
