# Quality Guidelines — bench-harness

## 红线

1. **确定性**：合成数据必须可复现（固定 seed、禁用时间/随机源），否则 CI 内存趋势无对比意义。
2. **测量诚实**：帧率类无法稳定自动化的指标标注 best-effort / 手工验收，禁止假装自动化。
3. **预算即验收线**（D10）：RSS 断言超标 = 红；调整预算须改 DECISIONS.md 并说明理由。

## M8 多目标测试边界

- `multi_target_routing_spec.rs` 使用 Mock 平台依赖，验证的是：
  - 冷目标选择键包含 HWND，选择 Telegram 不激活微信；
  - targeted 注入序列不含 `VK_RETURN`；
  - 无目标时先写剪贴板再给出友好反馈。
- 它**不替代**真实 IM 的 exe/class/title/UIA/caret 验证，也不替代 `Idle`/`Browse` 内存采样。
- 若要宣称“真实 IM 闭环”，必须另立 `#[ignore]` 手动测试或本机实测记录。

## 测试要求

- harness 自身的红灯测试先行（生成器行数、采样器格式）；对 app 的 RSS 断言属于集成验收。
