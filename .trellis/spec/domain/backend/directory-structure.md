# Directory Structure — domain

## 布局

```
crates/domain/
├── Cargo.toml        # 仅 serde（后续若加 uuid 类型也不引外部依赖）
├── src/
│   └── lib.rs        # 全部实体 + 内联 #[cfg(test)] mod tests
```

单文件即可，类型增多时按主题拆模块（`asset.rs` / `query.rs`），但保持零 IO。

## 模块组织规则

- ID 新类型模式：`pub struct AssetId(pub u32);` 带 `Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize`（见 `AssetId`/`CategoryId`/`TagId`）。
- 值对象用 enum 承载组合语义：`Filter` 是谓词树（All/InCategory/HasTag/Not/AllOf/AnyOf），递归嵌套用 `Box<Filter>`。
- 排序规格与过滤解耦：`Sorter { keys: Vec<SortSpec> }`，多键稳定排序在 `sort_assets` 内实现。

## 命名约定

- 测试名 = 行为描述 snake_case：`sorter_recency_then_name_is_stable_multisort`、`smart_folder_serde_roundtrip_preserves_filter_sorter`。
- 测试夹具函数放 tests mod 顶部小写辅助（如 `fn asset(id, name, created_at)`）。
