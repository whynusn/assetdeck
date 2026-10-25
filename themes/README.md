# themes/ —— 主题包目录（即放即用）

每个子目录是一个主题包，目录名 = `theme.toml` 里的 `meta.id`。

```
themes/
  sakura-cat/
    theme.toml        必需。schema 见 automation/theme-system/theme-spec.md §3
    THIRD_PARTY.md    含 assets/ 时必需。素材来源 + 许可证表
    assets/           可选。该主题自带的插画/纹理/图标/音效
```

## 状态

当前目录只有 `_template/`（可复制的骨架，不是可加载的主题包 —— `id` 以 `_` 开头，
校验与扫描都应跳过 `_` 前缀目录）。

成品主题包由 `automation/theme-system` 自动化按 `state.json` 的 T1 模块逐个产出，
产出后登记进 `state.json.shippedThemes`。

## 许可纪律（对外发布前提）

- 允许：CC0-1.0 / Public Domain / OFL-1.1 / MIT / Apache-2.0 / CC-BY-4.0（须署名）。
- 禁止：任何商业 IP 角色形象或名称（含 Sanrio / Hello Kitty / Disney / 任天堂等）、CC-BY-NC、
  无许可证素材、来源不明素材、从壁纸站抓取的图。
- 命名不得使用 IP 名称或近似商标。可爱风主题走**原创形象**。
- 门禁：`node automation/theme-system/scripts/license-audit.mjs`（体积/许可证/来源/IP 关键词）。

## 边界（D55）

std-widgets（输入框、滚动条等 fluent 内置控件）只认明暗两档，每个主题包用 `meta.base` 声明
最接近的一档。自绘层（窗口底、面板、瓦片、按钮、菜单、弹窗）完全可定制。
字体族只能取**系统已安装**字体（Slint 1.17.1 无运行时字体注册），打包字体属待评估项。
