# Implement — M7 bench-harness

## 顺序清单(Red→Green→Refactor)

1. **生成器先行**
   - [ ] 红灯:`synthetic_library_generator_produces_100k_metadata_rows`(tests/generator_spec.rs):generate(tempdir,100k,thumbs=100) → Store::open 数 assets 表 == 100_000;抽查 uuid/确定性(两次生成 DB 行集全等);缩略图文件数 == 100 且路径符合 thumbnail_cache_path。
   - [ ] 绿灯:src/generate.rs + src/sampler.rs 骨架 + main.rs 子命令分发。
2. **闭环计时**
   - [ ] 红灯→绿灯:`closed_loop_doubleclick_to_input_box_under_500ms`(tests/closed_loop_spec.rs,cfg(windows)):按 design.md 时序;真实 Win32 剪贴板写+读回;CopiedOnly 断言;<500ms best-effort 注释。测试结束清剪贴板(Text 覆盖写空串)。
3. **app-ui --bench 模式**
   - [ ] main.rs 加 `--bench <root> --bench-hold-ms N` 分支:不开窗、建 VM、脚本化浏览、hold、JSON 输出、exit(0)。deps_guard 保持绿。
4. **RSS 断言(ignored)**
   - [ ] tests/rss_spec.rs:`idle_rss_under_100mb` / `browse_100k_rss_under_250mb` 标 #[ignore = "mem-regression job 与本地手动跑"];内部:定位 exe 缺失即 panic;spawn+采样+中位数断言预算。
   - [ ] 本地实测:`cargo build --workspace` 后 `cargo test -p bench-harness -- --ignored --nocapture` 两项通过并贴中位数字节。
5. **CI job 启用**
   - [ ] ci.yml mem-regression 替换占位为 design.md 形态;确认 YAML 语法(本地无 actionlint 则逐行人工核对缩进)。
6. **收尾验证**

## 验证命令(CI 同序)

```powershell
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace; cargo test -p bench-harness --release -- --ignored --nocapture   # RSS 实测
```

## 审查门

- 门 1(步骤 2 后):闭环测试读回校验真实发生(非仅无 Err);CopiedOnly 语义断言。
- 门 2(步骤 4 后):RSS 数字贴出且低于预算有余量(>10% 余量才算稳;贴边过=记录观察项);测量失败路径有测试或代码审阅证据(ProcessGone→红)。
- 门 3(全部后):三命令绿;既有 49 测试零改动。

## 回滚点

- 步骤 1–2 独立可合(harness 工具不影响产品 crate);
- CI job 若 runner 环境敌对(GUI/剪贴板):对应环节转 #[ignore] + notes 记录,CLI measure-rss(browse 模式不开窗)保底。

## 明确不做

- 趋势看板、跨平台采样、clap、真实渲染帧率、视频抽帧解码栈决策(仍另立任务)。
