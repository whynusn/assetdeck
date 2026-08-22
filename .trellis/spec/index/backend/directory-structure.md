# Directory Structure — index

## 布局

```
crates/index/
├── Cargo.toml          # deps: domain, roaring; dev-deps: proptest, criterion
├── src/
│   └── lib.rs          # FacetIndex + evaluate + tag_counts 缓存
├── tests/
│   ├── pipeline.rs     # 过滤管线行为测试
│   ├── oracle.rs       # proptest 对拍蛮力 oracle
│   └── budget.rs       # 性能预算断言（debug+release 双档）
└── benches/
    └── budget.rs       # criterion 基准（harness = false）
```

## 模块组织规则

- 核心结构体 `FacetIndex` 单文件；新增 facet 维度（如按扩展名/尺寸桶）= 新增 `HashMap<Key, RoaringBitmap>` 字段 + insert/remove 同步维护 + 计数缓存字段。
- **行为测试放 tests/（集成），纯数学/不变量可内联**——本 crate 的正确性契约跨 crate（依赖 domain::Filter），故用集成测试。

## 命名约定

- 预算测试统一后缀 `_within_budget_1ms_at_1m`；oracle 对拍测试统一 `facet_counts_match_bruteforce_oracle` 风格。
