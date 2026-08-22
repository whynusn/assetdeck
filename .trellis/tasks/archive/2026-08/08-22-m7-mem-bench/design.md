# Design — M7 bench-harness

## 边界

```
tools/bench-harness/
├── Cargo.toml    # deps: store, domain, ui-viewmodels, pipeline, platform,
│                 #      image(默认 features 关,仅 png 编码), windows-sys(ProcessStatus/Foundation/Threading), serde_json
├── src/
│   ├── main.rs       # 子命令分发(手写 arg 解析,不引 clap)
│   ├── generate.rs   # 确定性合成库生成器
│   └── sampler.rs    # GetProcessMemoryInfo WorkingSet 采样器
└── tests/
    ├── generator_spec.rs     # synthetic_library_generator_produces_100k_metadata_rows
    └── closed_loop_spec.rs   # closed_loop_doubleclick_to_input_box_under_500ms
crates/app-ui/src/main.rs      # + --bench <lib_root> --bench-hold-ms N 模式(哑渲染路径,不开窗也可用软件后端)
.github/workflows/ci.yml       # mem-regression job 启用
```

## 子命令契约

1. `generate --rows N --out DIR [--thumbs M]`
   - uuid = `format!("bench-{i:08}")`(TEXT 主键,确定性);file_name = `asset_{i}.png`;created_at = 基准+i;
   - 缩略图仅前 M 条(默认 2000):image png 编码 64×64 渐变 → `store::thumbnail_cache_path` 落盘;M7 断言只数元数据行,缩略图子集化保证「秒级生成」(spec database-guidelines 承诺)——偏离记录见下;
2. `measure-rss --exe PATH --library DIR --mode idle|browse --budget-mb B --hold-ms H`
   - spawn 子进程:`idle` 模式无参数直接启动;`browse` 模式传 `--bench DIR`;
   - 每 250ms 采样 WorkingSet(GetProcessMemoryInfo),丢弃前 8 个样本预热,取中位数;
   - stdout 输出单行 JSON `{"mode":"…","median_bytes":N,"samples":K,"budget_bytes":B}`;超预算 exit 1;子进程非零/提前退出 → exit 2(测量失败=红);
3. `closed-loop`:跑一次闭环计时并输出 JSON(harness 内建,供 CI 日志)。

## app-ui --bench 模式

- `--bench <root>`:Store::open(root/meta.db) → 读全量 AssetMeta → 建 FacetIndex(uuid→顺序 AssetId 映射)+ LibraryGridVm(aspect 由 id 导出,M5 既定 stub)→ 脚本化浏览:每 5000 项跳一窗 ensure_window,循环至尾再回首 → 进入 hold 静置(`--bench-hold-ms`,默认 8000)供父进程采样 → println 一行 JSON 结果 → exit(0)。
- 不创建窗口(Slint 仅在无 --bench 时 run())——内存守卫测的是 VM 数据结构,诚实标注「不含真实渲染器驻留」;真实 GUI 空闲由 idle 模式覆盖。
- main.rs 保持薄壳:--bench 分支独立函数,业务逻辑仍住 VM/harness。

## RSS 采样器(sampler.rs)

```rust
pub fn working_set_bytes(pid: u32) -> Option<u64>;          // OpenProcess+GetProcessMemoryInfo
pub fn sample_median(pid: u32, poll_ms: u64, warmup: usize) -> Result<SampleReport, SamplerError>;
```
- SamplerError 变体:ProcessGone / ApiFailed — 上层一律转红。

## 闭环计时(closed_loop_spec.rs)

```
LibraryGridVm(小合成库) --double_click--> take_events[OpenAsset]
  → negotiate(Image→Png bytes 或 Text) 
  → Win32ClipboardSink::write(真实剪贴板) + 读回校验(GetClipboardSequenceNumber 变化或读回比对)
  → MockFocusWatcher 死窗口 → CopiedOnly
Instant 全程 < 500ms(best-effort 注释)
```
- 平台门:#[cfg(windows)] 整个测试文件(CI=windows-latest 恒真)。
- 若 CI 会话剪贴板不可用导致红 → 回退预案:改 #[ignore] 并在 notes 记录理由(诚实降级,不许静默放宽预算)。

## 四个测试的形态

| 测试 | 形态 | 运行时机 |
|---|---|---|
| synthetic_library_generator_produces_100k_metadata_rows | #[test] 常规 | 每次 cargo test(tempdir,~秒级) |
| closed_loop_doubleclick_to_input_box_under_500ms | #[test] 常规 cfg(windows) | 每次 cargo test |
| idle_rss_under_100mb | #[ignore] 集成测 | mem-regression job / 本地手动 |
| browse_100k_rss_under_250mb | #[ignore] 集成测 | 同上 |

- ignored 测试内部:先定位 app exe(`<workspace>/target/<profile>/asset-manager.exe`,profile 取 `cfg!(debug_assertions)`;要求 workspace 已 build——mem-regression job 先 cargo build --workspace;本地全量 cargo test --workspace 亦满足),缺失即 panic(测量失败=红)。

## ci.yml mem-regression job

```yaml
mem-regression:
  runs-on: windows-latest
  steps:
    - checkout / toolchain(stable) / rust-cache
    - run: cargo build --workspace
    - run: cargo test -p bench-harness --release -- --ignored --nocapture
    - run: cargo run -p bench-harness -- generate --rows 100000 --out ${{ runner.temp }}/synth-lib
    - run: cargo run -p bench-harness -- measure-rss --exe target/release/asset-manager.exe --library ${{ runner.temp }}/synth-lib --mode idle --budget-mb 100
    - run: cargo run -p bench-harness -- measure-rss --exe target/release/asset-manager.exe --library ${{ runner.temp }}/synth-lib --mode browse --budget-mb 250
    - uses: actions/upload-artifact@v4 (趋势 JSON)
```
- release 档跑 RSS(D10 合同以发布形态为准);ignored 测试与 CLI 双保险。

## 权衡记录

| 决策 | 备选 | 理由 |
|---|---|---|
| ignored 测试走 target/debug、CLI 走 release | 统一 release | 本地 cargo test 无 release 产物;debug 断言更保守(超预算更早暴露),CI job 用 release 出正式数字 |
| 缩略图子集生成(M 默认 2000) | 全量 100k 张 | 「秒级生成」承诺;浏览路径内存守卫只物化可见窗,全量缩略图对断言无增益 |
| 手写 arg 解析 | clap | 减依赖;子命令仅三个 |
| uuid 字符串 bench-{i:08} | uuid v4 | 确定性红线(spec quality-guidelines) |

## 兼容与回滚

- app-ui 改动限 main.rs 加分支;deps_guard 不动。
- CI 若 GUI/剪贴板环境敌对:相应测试按回退预案转 #[ignore] + notes 记录,mem-regression 的 CLI measure-rss 路径不依赖窗口(browse 模式不开窗),保底可用。

## 测试钩子先例

- SampleReport/JSON 单行输出面向 CI 解析(spec logging-guidelines);harness 自身红灯先行(generator 行数断言)。
