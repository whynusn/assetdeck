# Directory Structure — pipeline

## 布局（M6 目标形态，当前为占位）

```
crates/pipeline/
├── Cargo.toml    # deps: domain; platform(trait) —— 不依赖 win32 实现
└── src/lib.rs
```

## 管线阶段（D8 锁定，不可重排）

```
触发(双击/热键) → 资产解析 → 格式协商(CF_HDROP/PNG/DIB/text) → 剪贴板写入 → 焦点校验 → 合成 Ctrl+V → [开关] 合成 Enter
```

- 每阶段一个纯函数或 trait 边界；格式协商必须**表驱动**（资产类型 × 目标 profile → 剪贴板格式）。
- auto-send 是管线末端独立布尔开关，**默认关**——任何重构不得把它并进主路径。

## 命名约定

- 测试名对齐 TDD_PLAN M6 清单：`format_negotiation_table_image_video_text`、`focus_check_failure_degrades_to_copy_only`、`auto_send_flag_defaults_off` 等。
