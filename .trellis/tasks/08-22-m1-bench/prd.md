# M1 基准测试：1M 位图交集性能验收

## 背景

`DECISIONS.md` D4 验收线：分类/属性过滤使用 RoaringBitmap 位图交集，在 100 万资产规模下求值必须 <1ms。M1 已交付 `FacetIndex` 与功能测试；本任务补齐性能验收与可重复基准。

## 需求

1. criterion 基准（`crates/index/benches/budget.rs`），覆盖三条路径：
   - `Filter::All` 全集求值（位图容器树克隆）
   - `AllOf` 双分类交集（各约数千成员）
   - 单分类查询基线
2. CI 兼容的预算断言集成测试（`crates/index/tests/budget.rs`）：
   - 构建百万级合成资产索引
   - 交集路径采样均值 < 1ms（预热后多次采样取均值，抗单次抖动）
3. 基准可本地复现：`cargo bench -p index`

## 验收标准

- [ ] `cargo test -p index --test budget` 通过
- [ ] `cargo bench -p index` 可运行并输出结果
- [ ] 全工作区 fmt / clippy -D warnings / test 三绿
- [ ] TDD_PLAN.md 对应项勾选并提交

## 范围外

- 持久层（M2）、内存回归 harness（M7）、真实 UI 渲染帧率
