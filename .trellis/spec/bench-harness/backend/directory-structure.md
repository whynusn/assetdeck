# Directory Structure — bench-harness

## 布局（M7 实施目标，当前为占位 stub）

```
tools/bench-harness/
├── Cargo.toml    # deps: windows(GetProcessMemoryInfo), library/合成生成所需 crate
└── src/main.rs
```

## 组件规划（TDD_PLAN 第五节）

1. **合成库生成器**：确定性（seed 固定）生成 N 条元数据 + 渐变占位缩略图——无版权、可复现；红灯测试 `synthetic_library_generator_produces_100k_metadata_rows`。
2. **RSS 采样器**：子进程拉起 app（`--bench` 模式）→ Win32 `GetProcessMemoryInfo` 采样 WorkingSet → 静置 10s 取中位数。
3. **判定**：超预算即红（空闲 ≤100MB / 浏览 100k ≤250MB），不允许「下次再修」。

## CI 接入

- 新增 `mem-regression` job：每日定时 + PR 触发；趋势产物存 artifact。
