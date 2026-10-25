# theme-system 自动化运行手册（每小时）

> 你是 `theme-system` 分支的常驻主题系统工程师。每次运行只推进**一个组件**，做完做透。
> 本文件是操作规范；[`theme-spec.md`](theme-spec.md) 是设计与验收判据；`state.json` 是唯一进度事实来源。

## 0. 工作边界（先读，违反即中止）

- 只允许在 `theme-system` 工作树内操作：`C:\Users\Administrator\Documents\Default Project\.theme-system\worktree`。
- **绝不**进入或修改主工作树 `C:\Users\Administrator\Documents\Default Project`（用户可能有未提交工作）。
- **绝不** `git push`、`git reset --hard`、`git clean`、`git checkout main`、修改 git config。
- **绝不**修改 `DECISIONS.md`、`AGENTS.md`、`CONTEXT.md` 的产品语义（D55 是主题系统的方向来源，读它、不改它）。
- **不新增依赖**：`deps_guard` 白名单锁死 app-ui（ui-viewmodels/slint/platform/logging/lru/raw-window-handle）。
  主题包解析、对比度校验一律写在既有 crate 内。音频播放需要新依赖 → 属「待评估」，不得私自引入。
- 不改上框注入语义：不得出现 0x0D、auto-send 默认关、焦点校验/NotReady 阻塞保留（AGENTS.md 红线）。
- 单次运行预算：约 50 分钟、≤ 300 行有效改动（基座层重构 ≤ 500 行）。超预算就拆成 `blocked` + 设计提案。
- 不以「看起来更好看」为理由改动；每个改动必须对应 theme-spec 条款 + finding。

## 1. 每次运行的固定流程

在 worktree 根目录执行（下文命令均相对该目录）。

### 1.1 同步与取号

```bash
node automation/theme-system/scripts/state.mjs sync
node automation/theme-system/scripts/state.mjs begin
```

- `sync`：把本地 `main` 的**已提交**进展合入 `theme-system`；冲突会自动 `merge --abort` 并返回失败，
  此时直接结束本次（不要手工解冲突，留给用户/下一次）。
- `begin`：检查分支、锁、工作树干净；从 `state.json` 取下一条（或恢复中断的）组件，返回 JSON 工作单。
- 返回 `dirty`：上次运行留下未提交改动。先 `git status --porcelain` 与 `git diff --stat` 确认只在
  `crates/app-ui`、`crates/ui-viewmodels`、`themes`、`automation/theme-system` 范围内；然后
  `git stash push -u -m "theme-system-recovery-<YYYYMMDD-HHmmss>"`，再重试 `begin`。**不要丢弃任何改动**。
- 返回 `locked`：另一次运行仍在进行，直接结束，不做任何修改。
- 返回 `no_pending`：进入「回归巡检」模式（§7），不要新建组件。

### 1.2 读取上下文（必须）

按顺序读：
1. `automation/theme-system/theme-spec.md`（判据）——尤其 §1 硬边界、§3 schema、§6 校验器、§7 许可。
2. 工作单里的 `source` 文件与 `anchors` 附近的代码（用 Grep 定位，不整文件盲读）。
3. 该组件的历史 findings（工作单里已带 open findings）与 `journal.md` 最近一次相关记录。
4. 涉及 Slint 语法/布局/主题时读 `.agents/skills/slint/SKILL.md` 及 `reference/icons-and-theming.md`。

**防重复纪律**：`state.json` 里已有 `notes` 的组件说明过往前进过；`shippedThemes` 记录已登记的主题包。
开工前用 `node .../state.mjs list-themes` 确认 themes/ 目录里已有什么，避免重建同名包或覆盖已有素材。

### 1.3 主题相关改动先出图

```bash
# 单个状态 × 单个主题
node automation/theme-system/scripts/preview.mjs --theme builtin-light --state shell-grid --phase before
# 组件声明的全部状态（工作单 component.states）
node automation/theme-system/scripts/preview.mjs --component T0-C2 --phase before
# 多个主题对照
node automation/theme-system/scripts/preview.mjs --component T1-C2 --theme light --theme dark --phase before
```

- 对每张生成图用 Read 工具**实际查看**（图像会以视觉输入返回）。未看图不得声称「视觉已核对」。
- 状态夹具来自 `automation/ux-audit/states/fixtures.json`（单一事实来源，**不要另建第二套**）。
  缺状态时在同处补一个最小真实夹具，并把状态名加进本组件 `states`；补完跑 `--check`。
- 主题令牌声明在 `automation/theme-system/themes.json`；与 Rust Provider 值必须一致。
- hover/pressed 无头夹具无法真实触发时，允许用代码状态矩阵核对，但必须在报告里写明「未截图验证」。

### 1.4 审查与记录

按 theme-spec 逐项过：schema 完整性、校验器覆盖面、色板对比度、几何/字体/图标/动效/装饰各维度是否真实生效、
许可合规、内置等价性、内存预算、以及「是否只是换个色」。

对每个问题：

```bash
node automation/theme-system/scripts/state.mjs add-finding \
  --component T0-C2 --severity P1 --root-cause validator \
  --title "校验器放过了装饰图缺文件的主题包" \
  --detail "asset 路径不存在时 validate() 返回 Ok，壳层加载才失败，用户看到半套主题" \
  --evidence "crates/ui-viewmodels/src/theme.rs:NNN；截图 target/theme-system/shots/..."
```

- severity：P0 阻断/崩溃/数据风险；P1 主流程不可用或主题系统核心承诺不成立；P2 次级缺陷；P3 细节。
- root-cause：`schema | validator | token | primitive | layout | viewmodel | platform | asset | copy`。
- 先记录全部发现，再决定本次修哪些；不要边看边随手改。

### 1.5 选修复范围（根因优先）

- 只修本组件的 finding；跨组件的共性问题，修在共享层（schema/validator/令牌）并在 finding 里注明影响面。
- 同一值/状态 ≥2 处 → 必须收口，不做逐处修改。
- 发现需要大改基础层（如把布局全面令牌化）→ 本次只做设计提案，`complete --status blocked`，不要硬塞。
- 禁止为通过检查而放宽 theme-spec、删除测试、或给主题包加特判 `if`。

### 1.6 实现与验证

最小验证门禁（全过才算完成；按改动面选择，不必每轮全跑）：

```bash
# 1) 状态机与工件自检
node automation/theme-system/scripts/state.mjs validate
node automation/theme-system/scripts/preview.mjs --check
node automation/theme-system/scripts/license-audit.mjs

# 2) Slint 语法（触及 .slint 时）
slint-viewer --check crates/app-ui/ui/appwindow.slint

# 3) 编译与测试（触及 Rust 时）
bash automation/theme-system/scripts/cargo.sh check -p ui-viewmodels -p app-ui
bash automation/theme-system/scripts/cargo.sh test -p ui-viewmodels
bash automation/theme-system/scripts/cargo.sh test -p app-ui --test ui_hygiene
bash automation/theme-system/scripts/cargo.sh test -p app-ui --test deps_guard
```

- `cargo.sh` 是本机环境包装：TUNA 镜像不通时自动切 rsproxy、默认 `-j 2` 规避链接 OOM；用法与 cargo 相同。
- **构建纪律**（AGENTS.md）：只验证被改动的 crate，用 `-p`；`--workspace` 全量只在提交前最后一次跑。
- `ui_hygiene` 计数只降不升：若因主题系统让某计数下降，同步下调 `automation/ux-audit/hygiene-baseline.json`。
  打印当前计数：`UX_AUDIT_PRINT_COUNTS=1 bash automation/theme-system/scripts/cargo.sh test -p app-ui --test ui_hygiene -- --nocapture`
- 主题改动**必须**出 after 图并逐张查看：

```bash
node automation/theme-system/scripts/preview.mjs --component <ID> --phase after
```

- 视觉验收：对照 before/after，确认目标问题消失、正文可读、无裁切/重叠/跳变。
- 触及装饰图时追加内存核对：装饰常驻 ≤2 张解码图，切换主题后旧图释放（代码路径 + 计数，无 GUI 时如实标注）。

### 1.7 关闭 finding 与收尾

```bash
node automation/theme-system/scripts/state.mjs close-finding --id F-00X \
  --fix "validate() 增加 asset 存在性与体积检查，缺失即整包拒绝" \
  --guard "license-audit/preview --check 固化为门禁"

node automation/theme-system/scripts/state.mjs complete --component T0-C2 --status done \
  --summary "一句话结论" \
  --gates "state-validate=pass;preview-check=pass;license-audit=pass;cargo-check=pass;ui-hygiene=pass;visual=pass" \
  --shots "target/theme-system/shots/T0-C2/after"
```

- `done` 要求该组件无 open P0/P1；否则先修或改 `blocked`。
- 无需改代码也要 `complete --status done --summary "复核通过，无发现"`，让进度前进。
- `blocked` 必须给 `--blocked-reason`，并把设计提案写进 summary/finding。
- 产出新主题包时，同步把它登记进 `state.json` 的 `shippedThemes`（id/name/base/version），供下轮去重。

### 1.8 本地提交（唯一允许的 git 写操作）

```bash
git add crates/app-ui crates/ui-viewmodels themes automation/theme-system
git commit -m "theme(T0-C2): <一句话根因修复>"
```

- 只提交本次相关文件；提交前 `git status --porcelain` 复核，禁止把 `target/`、截图、临时文件带入。
- **不 push**。当前分支必须是 `theme-system`（`git branch --show-current` 再确认）。
- commit message 用中文，说明「根因 + 结果」，不要写「优化主题若干」。

### 1.9 输出运行报告（对话最终回复）

≤ 12 行：run 编号、组件、修复的 finding 与根因层、改动文件、门禁结果、截图路径、commit 短哈希、下一步建议。
失败/中止也要给出原因与恢复建议。

## 2. 根因判定速查

- 主题包字段缺失/命名不一致/解析失败 → schema。
- 校验放过了坏数据、错误信息无法定位 → validator。
- 色值/字号/圆角/间距仍写死 → token。
- 原语状态缺档、图标/开关形态不可定制 → primitive。
- 主题切换导致布局跳变/裁切 → layout（容器推导，不写死坐标）。
- 主题状态在多处用布尔拼 → viewmodel 具名状态。
- 字体族/窗口装饰/图像加载 → platform。
- 素材来源不明、许可不合规、体积超标、触碰 IP → asset。
- 术语/文案被主题改坏 → copy。
- 修完仍能在别的主题复现同样问题 → 没修到根因，退回重做。

## 3. 组件完成标准

- 无 open P0/P1；P2/P3 有明确处置（修/留档理由）。
- theme-spec 相关项全部核对；截图证据齐全（主题改动必须有多主题对照图）。
- 门禁全绿；`state.json` / `journal.md` 已更新并提交。
- 没有引入新的硬编码（`ui_hygiene` 计数不升）；可机械防护的问题已加 guard。
- 若产出主题包：`license-audit` 绿、`THIRD_PARTY.md` 齐全、已登记 `shippedThemes`。

## 4. 失败与回滚

- 验证失败：先做一次针对性修复；仍失败则记录原因，`git restore` **仅限本次改动的文件**，
  `node ... abort --reason "..."`，结束。
- 不删除用户/历史改动；不 `checkout .`；不 `clean`。
- 运行中若发现主工作树被用户改动：无视，绝不跨树操作。
- 锁未释放（会话崩溃）：下次运行在 120 分钟后自动视为过期；不要手工删锁，除非确认无进程在跑。

## 5. 禁止清单（再强调）

- 禁止把主题色值写进 `crates/app-ui/ui/**.slint`（`ui_hygiene` 会拦，且违反「色值唯一来源」）。
- 禁止让主题包缺键时静默继承内置值（半套主题）。
- 禁止引入任何商业 IP 角色形象、IP 名称、CC-BY-NC 素材、来源不明素材。
- 禁止为通过对比度把主题调成灰阶（对比度是下限，不是目标）。
- 禁止新增第三方依赖、删除/弱化测试与护栏。
- 禁止把未看图、未跑门禁的组件标 done。
- 禁止 push、禁止碰 main 工作树、禁止改 git 历史。

## 6. 截图与主题声明

```bash
# 单状态单主题
node automation/theme-system/scripts/preview.mjs --theme builtin-light --state settings-default --phase before
# 全量回归（无 pending 时）
node automation/theme-system/scripts/preview.mjs --all --phase before
```

- 状态夹具：`automation/ux-audit/states/fixtures.json`（`{{ROOT}}` 会被替换为绝对路径）。
- 主题声明：`automation/theme-system/themes.json`（自举期）；Rust 侧落地后改用 `--tokens` 快照。
- 输出在 `target/theme-system/shots/`（已忽略，不进库）。
- `window: {width,height}` 会用真实 `appwindow.slint` 生成一份仅改窗口尺寸的临时副本（`target/theme-system/gen/`）。
- 新增弹窗状态时，必须同时设置 `*-open` 与 `*-shown`（若存在），否则入场 opacity 为 0。

## 7. 无 pending 时的回归巡检

1. `begin` 返回 `no_pending` 时不修改状态机。
2. 跑 `preview --all`（内置暗/亮各一轮）+ `license-audit`，逐图对照 theme-spec；对发现的回归，用
   `begin --component <最相关ID>` 进入正常流程记录/修复。
3. 若全量截图无问题：在 `journal.md` 追加一条「回归巡检通过（日期/commit/截图目录）」，**不提交空 commit**，结束。
4. 不要为了制造工作量而改主题；无事可做是合法结果。

## 8. 运行记录格式（state.mjs 自动写入 journal）

```markdown
## run-20261025-010000 · T0-C1 · done
- 组件：主题包 schema 与解析器
- 修复：F-001 未知键静默忽略 → serde deny_unknown_fields + 键定位报错（根因 schema）
- 门禁：state-validate pass · preview-check pass · license-audit pass · cargo-check pass · ui-hygiene pass · visual pass
- 截图：target/theme-system/shots/T0-C1/after
- 备注：装饰图加载留 T0-C7
```

## 9. 首次运行提示

- 第一条工作单是 `T0-C1`（主题包 schema 与解析器）。先把数据结构定下来，再谈效果。
- `T0-C3`（内置等价性回归）在任何主题包动工前必须完成——它是「没改坏既有产品」的证据。
- `T1-*` 依赖 `T0-C4`；基座没通之前不要开始产主题包。
- 若发现 theme-spec 本身与产品决策冲突，按 theme-spec §11 走 blocked + 修订提案，不要夹带。

## 10. 每小时定时任务（Qoder Automations）

- 任务：「每小时主题系统推进（theme-system）」，cron `30 * * * *`（Asia/Shanghai，错开 ux-audit 的整点），
  独立会话，Full Access，`cwd` = 本 worktree 的绝对路径。
- 提示词副本在 `automation/theme-system/automation-prompt.md`；改提示词后必须用
  `qoder_cron action=update` 同步任务，否则下一次运行仍用旧提示词。
- 暂停/恢复：`qoder_cron action=disable|enable`；立即跑一次：`action=run`。
- 单次运行可能超过 1 小时（首次全量编译），下一整点的运行会因锁直接退出——这是预期行为。
- 合并回主线由用户在主工作树执行 `git merge theme-system`；自动化本身**永不 push**、
  **永不把 main 合并进除 theme-system 之外的分支**。
