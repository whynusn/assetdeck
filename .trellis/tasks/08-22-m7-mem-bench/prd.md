# PRD — M7 内存回归与闭环验收

> 依据:TDD_PLAN M7 + DECISIONS.md D10(行动项 A2/A3 收口)。预算是合同数字:空闲 ≤100MB / 浏览 10 万条 ≤250MB。

## 需求(与 TDD_PLAN M7 一一对应)

1. `synthetic_library_generator_produces_100k_metadata_rows` — 确定性合成库生成器(常规测试,不 ignore)。
2. `idle_rss_under_100mb` — 子进程启动 app 静置采样 WorkingSet,中位数 ≤100MB(红线 D10)。
3. `browse_100k_rss_under_250mb` — 加载 10 万条合成库并驱动浏览脚本后采样,≤250MB。
4. `closed_loop_doubleclick_to_input_box_under_500ms` — 双击→粘贴管线完成端到端计时(行动项 A2 自动化部分,诚实标注边界)。
5. CI 启用 `mem-regression` job:预算超标即红,趋势产物存 artifact。

## 闭环测试的诚实边界(写进断言注释)

「双击→输入框」的真实端到端含 IM 目标窗口,无法诚实自动化。自动化部分 = `VM.double_click → VmEvent::OpenAsset → negotiate → 真实 Win32 剪贴板写入+读回校验 → 焦点校验降级 CopiedOnly`(CI 无 IM 目标)。真实 SendInput 进输入框由 `real_sendinput_into_notepad`(#[ignore])人工补全。

## 约束

- bench-harness 是独立工具 crate(tools/):可依赖 store/domain/ui-viewmodels/pipeline/platform/image(仅 PNG **编码**,生成占位缩略图;不解码用户资产——UI 不解码红线指 UI 进程路径,工具 crate 豁免但需在 spec 记录理由)。
- 测量失败 = 红(harness spec error-handling):子进程提前退出/采样 API 失败按超预算处理,禁止静默跳过。
- RSS 断言用宽裕采样纪律:预热丢弃前段、取中位数、多轮稳定。
- 三命令全绿保持;ignored 测试默认跳过,由 mem-regression job 显式跑。

## 范围外(明确不做)

- 真实渲染帧率自动化(TDD_PLAN 第六节);趋势可视化看板;跨平台内存采样。

## 验收标准

- 四个红灯测试就位:1 个常规绿、2 个 ignored(RSS)、1 个常规绿(闭环);
- 本地手动跑 `cargo test -p bench-harness --release -- --ignored` 两项 RSS 断言实测通过;
- ci.yml mem-regression job 启用且语法正确(actionlint 或本地无工具则人工核对);
- 三命令全绿。
