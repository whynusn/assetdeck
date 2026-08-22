# Quality Guidelines — domain

## 红线（违反 = 架构破坏）

1. **零 IO、零依赖基础设施**：不得 use std::fs/net，不得依赖 store/library/index 等 crate。domain 是依赖图的根。
2. **所有公共类型必须 `Serialize + Deserialize`**：Filter/Sorter/SmartFolder 要进 store 序列化与未来 IPC 协议（见 `SmartFolder` serde roundtrip 测试）。
3. **禁止 Slint 类型渗入**：本 crate 永远不依赖 slint。

## 必做模式

- ID 一律新类型（`AssetId(pub u32)`），禁止裸 u32 在 API 边界流动——index 层位图用 `.0` 显式降级。
- 排序必须稳定多键（`sort_by` + 逐键比较返回 Equal 时保持原序）。

## 测试要求

- 行为测试内联在 `#[cfg(test)] mod tests`；每个公共方法至少一条行为测试。
- serde roundtrip 测试对每个可序列化公共类型必做（参照 `smart_folder_serde_roundtrip_preserves_filter_sorter`）。

## Code Review 清单

- [ ] 新类型是否带全 derive 集？
- [ ] 是否引入了 IO 或外部服务概念？
- [ ] Filter 树新增变体时 index 层 evaluate 是否同步扩展？（跨 crate 联动）
