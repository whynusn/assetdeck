# Quality Guidelines — bench-harness

## 红线

1. **确定性**：合成数据必须可复现（固定 seed、禁用时间/随机源），否则 CI 内存趋势无对比意义。
2. **测量诚实**：帧率类无法稳定自动化的指标标注 best-effort / 手工验收，禁止假装自动化（TDD_PLAN 第六节诚实清单）。
3. **预算即验收线**（D10）：RSS 断言超标 = 红；调整预算须改 DECISIONS.md 并说明理由。

## 测试要求

- harness 自身的红灯测试先行（生成器行数、采样器格式）；对 app 的 RSS 断言属于集成验收。
