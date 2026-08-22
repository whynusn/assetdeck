# PRD — M6 粘贴管线

> 依据:TDD_PLAN M6 清单 + DECISIONS.md D8/D12。红线密集里程碑;七个测试(六自动 + 一 ignore)按清单验收。

## 需求(与 TDD_PLAN M6 一一对应)

1. `format_negotiation_table_image_video_text` — 表驱动协商:资产类型 × 目标 profile → CF_HDROP/PNG/DIB/text。
2. `paste_writes_clipboard_before_focus_switch` — 共享操作日志断言:写剪贴板严格先于焦点校验/注入。
3. `focus_check_failure_degrades_to_copy_only` — mock WindowProvider 返回死窗口 → 结果 CopiedOnly,零注入(红线 D8)。
4. `auto_send_flag_defaults_off` — 配置默认值快照测试(红线:回车直发默认关)。
5. `auto_send_off_never_synththesizes_enter` — 关开关时注入序列不含 VK_RETURN。
6. `previous_foreground_window_recorded_on_panel_invoke` — 面板唤起时记录「前一前台窗口」句柄。
7. `real_sendinput_into_notepad` — `#[ignore]`,本地手动跑,CI 不跑。

## 红线(每条必须有守卫)

- auto-send 是管线末端独立布尔开关,**默认关**;任何重构不得并入主路径(D8)。
- 焦点校验失败 = 降级仅复制 + 可呈现的 outcome(供 UI toast),**不是 Err**(pipeline spec error-handling)。
- 写剪贴板必须先于切焦点/注入(顺序颠倒会粘出旧内容)。
- platform 的 trait 零依赖(win32 实现整体 cfg(windows));业务 crate 不得出现 SendInput/CF_* 字样(platform spec quality)。
- UI 进程不解码:DIB 载荷只接受上游提供的已编码字节,platform/pipeline 不引入 image 解码依赖。

## 约束

- pipeline 依赖 domain + platform(trait);platform lib 零依赖、win32 模块用 windows-sys(已在 worker 验证 gnu 兼容)。
- windows-gnu 编译通过;clippy -D warnings 全绿。
- 不修改既有 crates 的代码与测试。

## 范围外(明确不做)

- UI 接线(双击素材 → 触发管线):M7 闭环做;
- 真实 IM 兼容矩阵/UIPI 实测(TDD_PLAN 第六节人工清单);
- 剪贴板监听/历史等增强。

## 验收标准

- 六个自动化测试全绿 + `#[ignore]` 测试存在且默认跳过;
- 三命令全绿;win32 模块在 gnu 下编译通过(cargo clippy --workspace --all-targets 已含);
- platform trait 层可脱离 Windows 编译的概念验证:trait 文件无 cfg 门、无 win32 import。
