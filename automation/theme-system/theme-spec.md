# 主题系统规格（theme-system 判据）

> 本文件是 `theme-system` 自动化的设计与验收判据。与 `AGENTS.md` / `DECISIONS.md` 冲突时以后者为准。
> 主题系统的产品方向由 **D55** 定盘（外置 TOML 主题包 + token 补全，标注「批 2」未实施）；本文件把 D55
> 展开成可执行规格。**只能按 §11 收紧，不能为让问题通过而静默放宽。**

## 0. 产品目标（用户原话转译）

1. **一套主题系统**：扩展新主题的成本低（加一个目录即生效），定制差异大（不是换个主色就完事）。
2. **元素覆盖广**：色板、几何、字体、图标、动效、背景装饰、卡面、声音都在主题控制范围内。
3. **成套主题包**：系统建成后持续产出成品主题，首个标杆是「粉色可爱猫咪」类原创风格。
4. **可对外发布**：素材许可必须干净，不碰商业 IP。

## 1. 硬边界（先读，违反即中止）

| 边界 | 事实 | 处置 |
|---|---|---|
| std-widgets 只认明暗两档 | fluent 样式的内部配色是 `Palette.color-scheme` 的活绑定（D37） | 每个主题包声明 `base = "dark"\|"light"`，std-widgets 跟随最近明暗档；**如实记录**，不假装全定制 |
| 无运行时字体注册 | Slint 1.17.1 无 `register_font_from_path`（已实测确认） | 字体族只能取**系统已装**字体；打包字体列为「待评估」项，不得假装支持 |
| 不新增依赖 | AGENTS.md + `deps_guard` 白名单锁 app-ui | 主题包解析走 `ui-viewmodels`（已有 serde + toml）；音频播放需新依赖 → 默认搁置 |
| 内存预算 | 空闲 ≤100MB、10 万条浏览 ≤250MB | 装饰图懒加载 + 尺寸上限 + 缓存计数；主题切换不得累积驻留 |
| 窗口标题栏不随主题 | D37 附注：chrome 跟随系统 | 不承诺 |
| 色值只在 `theme.slint` | `ui_hygiene` 扫描 `crates/app-ui/ui/*.slint`，仅豁免 `theme.slint` | 主题包色值**绝不落 slint 源文件**，只能经 Rust 注入 `Theme` 全局 |

## 2. 架构分层（目标形态）

```
themes/<pack-id>/theme.toml      用户/随附主题包（即放即用）
themes/<pack-id>/assets/*        该包自带的插画/纹理/图标（可选）
themes/<pack-id>/THIRD_PARTY.md  素材来源 + 许可证 + 作者（有 assets 时强制）

ui-viewmodels/src/theme.rs       ThemeTokens（已有）+ ThemePack 解析 + 校验 + 注册表
                                 ↑ 纯逻辑，无 I/O、无平台调用、可单测
app-ui/src/main.rs               apply_theme：ThemePack → slint 全局属性 / Window 默认字体 / 装饰图
crates/app-ui/ui/theme.slint     令牌定义（颜色 in-out，几何令牌逐步 in-out 化）
```

**数据流**：`theme.toml` →（serde）`ThemePack` →（校验器，含对比度门禁）→ `ThemeTokens` + `Geometry` +
`Typography` + `Decoration` →（壳层）注入 slint。校验失败的包**整包拒绝并降级到内置 dark**，不允许半套生效。

## 3. 主题包 schema（v1）

```toml
[meta]
id = "sakura-cat"            # 必填，^[a-z0-9][a-z0-9-]{1,31}$，与目录名一致
name = "樱花猫"
author = "assetdeck"
version = "1.0.0"            # semver
base = "light"               # dark | light，决定 std-widgets 档位与内置回退
description = "低饱和粉白、圆角卡片、角饰贴纸"
# license = "CC0-1.0"       # 包本身（非素材）的许可证，随附包必填

[palette]                    # 必填全集，缺一即校验失败（不允许隐式继承内置值）
bg-app = "#fbf7fa"
bg-app-alt = "#fbf7fa"       # 可选扩展：交替行/斑马纹底（缺省回退 bg-panel）
bg-panel = "#ffffff"
# …（完整清单见 §4.1；键名与 Theme 令牌一一对应，kebab-case）

[geometry]                   # 全部可选，缺省用内置基准
radius = "10px"
radius-lg = "16px"
border-width = "1px"
density = "comfortable"      # compact | comfortable | spacious → 缩放 Metrics/Spacing
control-height = "32px"      # 显式覆盖 density
row-height = "30px"
bar-height = "52px"
tile-radius = "12px"         # 瓦片专用圆角（可与面板不同）
tile-gap = "10px"

[typography]
font-family = "Microsoft YaHei UI"   # 必须是系统已装族；留空=系统默认
font-size-scale = "1.0"              # 0.9–1.2，整体缩放字号阶
display = "17px"                     # 可选：单档覆盖
letter-spacing = "0px"
line-height = "1.45"

[icons]
stroke-weight = "1.7"        # 描边粗细（Icon.weight 的默认值）
line-cap = "round"           # round | butt | square
size = "16px"
set = "line"                 # line | filled | pixel（filled/pixel 需自带 glyph 表，v2）

[motion]
enabled = true
fast = "100ms"
state = "120ms"
enter = "150ms"
easing = "ease-out"          # 契约只允许 ease-out / linear

[decoration]                 # 可选；全部相对包目录
background = "assets/bg.avif"        # 窗口底纹理/插画
background-tint = "#ffffff66"        # 压在图上的色罩，保正文可读
watermark = "assets/watermark.png"   # 右下角水印
watermark-opacity = "0.18"
corner = "assets/corner.webp"        # 角饰（默认右上）
corner-scale = "1.0"

[[decoration.stickers]]      # 可重复：任意贴纸，归一化坐标
image = "assets/sticker-1.webp"
x = "0.92"                   # 0.0–1.0，相对窗口宽
y = "0.08"
scale = "1.0"
opacity = "1.0"

[sound]                      # 可选；默认全关
enabled = false
click = "assets/click.ogg"
success = "assets/success.ogg"
```

**兼容纪律**：schema 只增不改。新增可选键＝向后兼容；新增必填键＝必须同时给迁移路径，否则不允许。
未知键**报错而不是忽略**（拼错键名必须当场暴露，不能静默半生效）。

## 4. 可定制维度矩阵（元素范围）

### 4.1 色板（38 令牌，必填全集）

`bg-app` `bg-panel` `bg-bar` `bg-raised` `bg-raised-hover` `bg-raised-press` `bg-input`
`line` `line-strong` `control-track-border`
`text` `text-2` `text-3`
`accent` `accent-hover` `accent-press` `accent-ink` `accent-soft`
`danger` `danger-soft` `danger-press` `danger-ink`
`warn` `warn-soft` `ok` `ok-soft` `shared` `shared-soft`
`bg-overlay` `scrim`
`label-bar-bg` `label-bar-bg-hover` `label-bar-text`
`badge-bg` `badge-text` `badge-ring` `tile-veil` `card-icon`

扩展（可选）：`bg-app-alt`（斑马纹）、`focus-ring`（缺省 accent）、`selection-bg`（缺省 accent-soft）。

### 4.2 几何与密度

圆角（面板/控件/瓦片各自独立）、描边宽度、控件高、行高、栏高、间距阶缩放、瓦片间距。
**density 只缩放尺寸，不改布局结构**——避免出现「主题能改坏布局」的耦合。

### 4.3 字体

字族、字号阶（5 档）、字重、字距、行高倍数。
实现通道 = `Window.default-font-family` / `default-font-size`（已实测可用）+ `Typography` 令牌覆盖。

### 4.4 图标

描边粗细、线帽/转角、尺寸、图标集选择（v1 只有 line；filled/pixel 需自带 glyph 表，v2）。

### 4.5 动效

三档时长 + 缓动。必须与「界面动画」开关叠加：开关关闭时主题动效同样归零（`Motion.enabled` 是总闸）。

### 4.6 背景与装饰（差异化的主战场）

窗口底纹理/插画 + 色罩、水印、角饰、任意贴纸（归一化坐标 + 缩放 + 透明度）。
**必须保证正文可读**：装饰只能出现在「无正文区域」或带色罩；`background-tint` 的对比度纳入门禁。

### 4.7 卡面与状态

瓦片圆角/间距/名称条形态/徽标形状/阴影档。状态线索必须保留非颜色冗余（形状/图标/文字）。

### 4.8 声音（默认关，暂不支持播放）

schema 预留，播放能力需新增音频依赖 → 属「待评估」，不得私自引入。

### 4.9 文案风味（可选，受术语约束）

只允许调整「语气短语」（如空态问候语），`CONTEXT.md` 的产品术语一律不得改。

## 5. 内置主题等价性（回归铁律）

内置 dark/light 必须长期可复现为**内置主题包**（`builtin = true`，编译期内嵌），且：

- 视觉输出与改造前**逐字节一致**（截图 hash 相同）——这是「主题系统没有改变既有产品」的唯一证据。
- 任何让内置暗/亮外观漂移的改动都必须显式记录理由，否则视为回归。

## 6. 校验器（运行时门禁，不是文档承诺）

`ThemePack::validate()` 必须拒绝：

1. 色板缺键 / 未知键 / 非法色值（非 `#rgb|#rrggbb|#rrggbbaa`）。
2. 对比度不达标（WCAG，与 `theme.rs` 现有测试同口径）：
   - `text-3` 对全部静态表面 ≥ 4.5:1
   - 语义色对 `bg-overlay` 与各自 `*-soft` ≥ 4.5:1
   - `label-bar-text` / `badge-text` 对「最坏缩略图」（纯白/纯黑）合成后 ≥ 4.5:1
   - `control-track-border` 对 `bg-overlay` ≥ 3:1
   - `danger-ink` 对 `danger-press` ≥ 4.5:1
3. 几何越界：`radius` 0–24px、`density` 枚举、`font-size-scale` 0.9–1.2、动效 ≤ 200ms。
4. 装饰资源缺失 / 超体积（单图 ≤ 512KB，包内资源合计 ≤ 4MB）。
5. `base` 与色板亮度自相矛盾（声明 light 但 `bg-app` 比 `text` 暗）→ 警告，不阻断。

校验错误必须能定位到**具体键**，不允许只说「主题无效」。

## 7. 素材许可策略（对外发布的前提）

**允许的许可证**：`CC0-1.0`、`Public Domain`、`OFL-1.1`（字体）、`MIT`、`Apache-2.0`、`CC-BY-4.0`（必须署名）。
**禁止**：任何商业 IP 角色形象（含 Sanrio / Hello Kitty / Disney / Pokémon / 各类动漫游戏角色）、
`CC-BY-NC`（不可商用）、无许可证素材、来源不明的 AI 生成图、从壁纸站抓取的图。

命名纪律：包名/目录名/描述**不得**使用 IP 名称或近似商标（如 `hello-kitty`、`sanrio`）。
「粉色可爱猫咪」类主题必须走**原创形象**：自制或开源素材，风格可近似但形象不复刻。

每个含 `assets/` 的包必须有 `THIRD_PARTY.md`：
```
| 文件 | 来源 URL | 作者 | 许可证 | 获取日期 |
|---|---|---|---|---|
| assets/bg.avif | https://... | ... | CC0-1.0 | 2026-10-25 |
```
机械门禁：`license-audit.mjs` 校验 THIRD_PARTY 存在性、许可证白名单、体积预算、IP 关键词黑名单。

## 8. 每轮运行的可交付判定

一轮完成必须同时满足：

1. 该组件在 `state.json` 里状态推进，且有可复述的结论。
2. 门禁全绿（见 runbook §4）。
3. 视觉证据：主题相关改动必须有「渲染截图 + 实际查看」，未看图不得声称视觉通过。
4. 无新增硬编码（`ui_hygiene` 计数不升）、无白名单外依赖（`deps_guard` 绿）。
5. 内置暗/亮等价性未被破坏（若本轮触及注入路径）。

## 9. 主题包产出标准（T1 模块的验收线）

一个主题包算「成品」需要：

- schema v1 全字段合法，校验器零错误。
- 至少 4 个状态截图（`shell-grid` / `shell-empty` / `settings-default` / 一个弹窗）实际查看通过。
- 差异幅度可感知：与内置主题相比，色板 + 至少 2 个其他维度（几何/字体/装饰/图标）同时变化。
- 正文可读性：装饰区不与正文重叠，或带足够色罩。
- 许可干净：`license-audit` 绿，`THIRD_PARTY.md` 齐全。
- 体感一致：切换主题后无布局跳变/裁切（对比截图）。

## 10. 常见失败模式（自动化应主动避开）

- 把主题色写进 `appwindow.slint` → 直接被 `ui_hygiene` 拦下，且违反「色值唯一来源」。
- 只为通过对比度而把色板调成灰阶 → 主题失去个性；对比度是下限不是目标。
- 主题包缺键时静默继承内置值 → 半套主题，排查成本极高；必须整包校验。
- 装饰图直接叠在瓦片区 → 正文不可读；装饰只能进空区或加色罩。
- 一次改太多维度 → 视觉回归无法归因；每轮只推进一个组件。
- 声称「支持打包字体」→ 当前 Slint 不支持，属于假承诺。

## 11. 规格收紧流程

若判据本身有问题（与产品决策冲突、或过严导致无法交付）：

1. 本轮不改判据，改为 `complete --status blocked` 并写清冲突点。
2. 在 `journal.md` 记录「判据修订提案」，由用户裁决。
3. 只有用户确认后才改本文件，且 `contractVersion` 递增。
