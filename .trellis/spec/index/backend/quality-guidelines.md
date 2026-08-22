# Quality Guidelines — index

## 红线

1. **禁止全量载入**：任何把百万级元数据读进普通 Vec 再过滤的实现违反 D3/D4。求值一律走 RoaringBitmap 交集/并集/补集。
2. **缓存变更即失效**：`tag_counts_cache` 在 insert/remove 中置 `None`。新增缓存字段必须遵循同一失效纪律，并由 `facet_count_cache_invalidates_on_tag_mutation` 式测试锁定。
3. **v1 禁向量检索**：不得引入 faiss/usearch 等（deny.toml bans + deps_guard 双守卫）。候选集抽象层预留接口即可。

## 必做模式

- 组合谓词求值为递归位图运算：AllOf=逐项交集、AnyOf=并集累积、Not=全集−子集（见 `FacetIndex::evaluate`）。
- 位图克隆仅在 API 边界（返回值），内部运算用引用借用（`&a & &cur`）。

## 测试要求（TDD 主战场）

| 类型 | 载体 | 示例 |
|---|---|---|
| 行为测试 | tests/pipeline.rs | `intersect_two_facets_returns_conjunction`、`negated_filter_excludes_ids` |
| 属性测试 | tests/oracle.rs | proptest 对拍蛮力 oracle（已抓「单资产重复标签」不变量违规） |
| 性能预算 | tests/budget.rs | debug+release 双档断言 @1M |
| 基准 | benches/budget.rs | criterion：交集 126µs / 单面 3.2µs / 全集 11.8µs |

- 领域不变量：**单资产的 tags 不得重复**——oracle 生成器必须主动构造重复标签验证防御。

## Code Review 清单

- [ ] 新 facet 维度是否在 insert/remove 双路径同步维护？
- [ ] 新缓存是否有失效测试？
- [ ] evaluate 新变体是否覆盖 domain::Filter 全部枚举？（match 穷尽即可编译期保证）
