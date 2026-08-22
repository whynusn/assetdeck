# Error Handling — pipeline

## 降级优先于报错（D8 红线）

- 管线的失败语义是**降级**而非中断：
  - 焦点校验失败 → 仅复制 + toast 提示，不注入；
  - 剪贴板写入失败 → 向上返回错误（这是唯一硬失败）。
- 目标窗口存活校验：用唤起面板时记录的「前一前台窗口」句柄；校验逻辑经 `WindowProvider` trait 抽象，测试注入 mock 死窗口。

## 错误形态

- 定义 `PipelineError` 枚举（Clipboard / AssetParse），实现 Display+Error；降级路径**不是** Err——用返回的枚举结果（如 `PasteOutcome::{Injected, CopiedOnly}`）表达，让调用方 UI 呈现 toast。

## 禁止

- 校验失败时「重试后强注」——宁可少发不可误发到错误窗口。
