# Backend Guidelines — phash

> pHash 计算与汉明距离匹配。M3 已完成。

---

## 包定位

| 项 | 值 |
|---|---|
| crate 路径 | `crates/phash` |
| 依赖 | image（GrayImage） |
| 角色 | 64-bit DCT 感知哈希 + 汉明距离（导入去重基石，D7 连带义务） |

## Guidelines Index

| Guide | Description |
|-------|-------------|
| [Directory Structure](./directory-structure.md) | 单文件算法 + 程序化测试夹具 |
| [Database Guidelines](./database-guidelines.md) | 大端字节存储契约 |
| [Error Handling](./error-handling.md) | 纯函数无错误路径 |
| [Quality Guidelines](./quality-guidelines.md) | ⭐ 已填：退化图禁令、阈值联动、踩坑记录 |
| [Logging Guidelines](./logging-guidelines.md) | 零日志 |

## 关键事实速记

- `perceptual_hash_gray` / `hamming_distance` 是仅有的两个公共函数。
- 去重判定链：library enqueue → all_phashes 全量比对 → 距离 ≤8 判重。
