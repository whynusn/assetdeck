# UX 审计自动化运行手册（每小时）

> 你是 `ux-audit` 分支的常驻 UI 打磨工。每次运行只做**一个组件**的深度审查与根因修复。
> 本文件是操作规范；`design-contract.md` 是审查判据；`state.json` 是唯一进度事实来源。

## 0. 工作边界（先读，违反即中止）

- 只允许在 `ux-audit` 工作树内操作：`C:\Users\Administrator\Documents\Default Project\.ux-audit\worktree`。
- **绝不**进入或修改主工作树 `C:\Users\Administrator\Documents\Default Project`（用户可能有未提交工作）。
- **绝不** `git push`、`git reset --hard`、`git clean`、`git checkout main`、修改 git config。
- **绝不**修改 `DECISIONS.md`、`AGENTS.md`、`CONTEXT.md` 的产品语义；不新增依赖；不引入 Slint 以外的 UI 框架。
- 不改上框注入语义：不得出现 0x0D、auto-send 仍默认关、焦点校验/NotReady 阻塞逻辑不动（见 AGENTS.md 红线）。
- 单次运行预算：约 50 分钟、≤ 250 行有效改动（基础层重构 ≤ 400 行）。超预算就拆成 `blocked` + 设计提案，不半成品交付。
- 不以“看起来更酷”为理由改动；每个改动必须对应契约条款 + finding。

## 1. 每次运行的固定流程

在 worktree 根目录执行（下文命令均相对该目录）。

### 1.1 同步与取号

```bash
node automation/ux-audit/scripts/state.mjs sync
node automation/ux-audit/scripts/state.mjs begin
```

- `sync`：把本地 `main` 的**已提交**进展合入 `ux-audit`；冲突会自动 `merge --abort` 并返回失败，此时直接结束本次（不要手工解冲突，留给用户/下一次）。
- `begin`：检查分支、锁、工作树干净；从 `state.json` 取下一条（或恢复中断的）组件，返回 JSON 工作单。
- 若 `begin` 返回 `dirty`：说明上一次运行留下了未提交改动。先 `git status --porcelain` 与 `git diff --stat` 确认只在 `crates/app-ui`、`crates/ui-viewmodels`、`automation/ux-audit` 范围内；然后 `git stash push -u -m "ux-audit-recovery-<YYYYMMDD-HHmmss>"`，再重试 `begin`。**不要丢弃任何改动**（stash 可恢复）。
- 若返回 `locked`：另一次运行仍在进行，直接结束，不做任何修改。
- 若返回 `no_pending`：进入“回归巡检”模式（§7），不要新建组件。

### 1.2 读取上下文（必须）

按顺序读：
1. `automation/ux-audit/design-contract.md`（判据）。
2. 工作单里的 `source` 文件与 `anchors` 附近的代码（用 Grep 定位，不整文件盲读 3800 行）。
3. 该组件的历史 findings（工作单里已带 open findings）与 `journal.md` 最近一次相关记录。
4. `.agents/skills/slint/SKILL.md` 及对应 reference（涉及布局/事件/主题时）。

### 1.3 审查前先截图

```bash
node automation/ux-audit/scripts/capture.mjs --component <ID> --phase before
```

- 对每张生成图用 Read 工具**实际查看**（图像会以视觉输入返回）。未看图不得声称“视觉已核对”。
- 采集范围 = `state.json` 里该组件的 `states`（单一事实来源）。缺状态时在 `states/fixtures.json` 补一个最小真实夹具，并把状态名加进该组件的 `states`；一次性排查可用 `--state <名字>` 临时加采，但不改登记。
- 新增夹具后跑 `node automation/ux-audit/scripts/capture.mjs --check`：它会校验「组件引用的状态都存在 / 每个夹具都有归属」，防止两套清单漂移。
- hover/pressed 无头夹具无法真实触发时，允许用代码状态矩阵核对，但必须在报告里写明“未截图验证”，不得含糊。

### 1.4 审查与记录

按契约逐项过：视觉层级/一致性、状态矩阵、间距与对齐、字体与截断、对比度、交互反馈、键盘/焦点、文案、空/错/忙态、真实数据压力。

对每个问题：

```bash
node automation/ux-audit/scripts/state.mjs add-finding \
  --component <ID> --severity P1 --root-cause token \
  --title "选中态用白色描边而非 accent" \
  --detail "瓦片 selected 时 border-color: #ffffffcc，与其他选中语义不一致" \
  --evidence "crates/app-ui/ui/appwindow.slint:1557；截图 before/M3-C3.png"
```

- severity：P0 阻断/数据风险/主流程不可用；P1 主流程一致性/交互摩擦；P2 次级打磨；P3 细节。
- root-cause：`token | primitive | layout | viewmodel | platform | copy`。
- 先记录全部发现，再决定本次修哪些；不要边看边随手改。

### 1.5 选修复范围（根因优先）

- 只修本组件的 finding；跨组件的共性问题，修在共享层（token/primitive）并在 finding 里注明影响面。
- 同一值/状态 ≥2 处 → 必须收口，不做逐处修改。
- 发现需要大改基础层（如组件拆分、全局焦点模型）→ 本次只做设计提案，`complete --status blocked`，不要硬塞。
- 禁止为通过检查而放宽契约、删除测试、加特判 `if`。

### 1.6 实现与验证

最小验证门禁（全过才算完成）：

```bash
slint-viewer --check crates/app-ui/ui/appwindow.slint
bash automation/ux-audit/scripts/cargo.sh check -p app-ui -p ui-viewmodels
bash automation/ux-audit/scripts/cargo.sh test -p app-ui --test deps_guard
bash automation/ux-audit/scripts/cargo.sh test -p app-ui --test ui_hygiene
node automation/ux-audit/scripts/capture.mjs --check
```

- `cargo.sh` 只是本机环境包装：TUNA 镜像不通时自动切 rsproxy、默认 `-j 2` 规避链接 OOM；用法与 cargo 相同，`--` 之后的参数原样传给测试二进制。

- 触及 `ui-viewmodels` 时追加：`bash automation/ux-audit/scripts/cargo.sh test -p ui-viewmodels`。
- 触及网格/滚动/内存敏感路径时，尽量跑相关 bench 或至少记录未跑原因。
- 修改后再截图并逐张查看：

```bash
node automation/ux-audit/scripts/capture.mjs --component <ID> --phase after
```

- 视觉验收：对照 before/after，确认目标问题消失且无新的裁切/重叠/跳变；暗色为主，浅色按组件要求另行核对。
- 若 `ui_hygiene` 计数下降：同步下调 `automation/ux-audit/hygiene-baseline.json`（只降不升）。打印当前计数：

```bash
UX_AUDIT_PRINT_COUNTS=1 cargo test -p app-ui --test ui_hygiene -- --nocapture
```

### 1.7 关闭 finding 与收尾

```bash
node automation/ux-audit/scripts/state.mjs close-finding --id F-00X \
  --fix "改为 Theme.accent，并统一瓦片选中描边" \
  --guard "ui_hygiene 限制新增十六进制色值"
```

然后：

```bash
node automation/ux-audit/scripts/state.mjs complete --component <ID> --status done \
  --summary "一句话结论" \
  --gates "slint-check=pass;cargo-check=pass;deps-guard=pass;ui-hygiene=pass;visual=pass" \
  --shots "target/ux-audit/shots/<ID>/after/*.png"
```

- `done` 要求该组件无 open P0/P1；否则先修或改 `blocked`。
- 无需改代码也要 `complete --status done --summary "复核通过，无发现"`，让进度前进。
- `blocked` 必须给 `--blocked-reason`，并把设计提案写进 summary/finding。

### 1.8 本地提交（唯一允许的 git 写操作）

```bash
git add crates/app-ui crates/ui-viewmodels automation/ux-audit
git commit -m "ux(<ID>): <一句话根因修复>"
```

- 只提交本次相关文件；提交前 `git status --porcelain` 复核，禁止把 `target/`、截图、临时文件带入。
- **不 push**。当前分支必须是 `ux-audit`（`git branch --show-current` 再确认）。
- commit message 用中文，说明“根因 + 结果”，不要写“优化若干样式”。

### 1.9 输出运行报告（对话最终回复）

≤ 12 行：run 编号、组件、修复的 finding、根因层、改动文件、门禁结果、截图路径、commit 短哈希、下一步建议。
失败/中止也要给出原因与恢复建议。

## 2. 根因判定速查

- 颜色/字号/圆角/间距写死 → token。
- 按钮/开关/菜单行缺状态或不一致 → primitive。
- 弹窗位置/宽度/高度/滚动/裁切 → layout（容器推导，不写死坐标）。
- 同一交互在多处用布尔组合拼 → viewmodel 具名状态。
- 窗口/焦点/层级/DPI → platform。
- 术语、错误提示、空态文案 → copy。
- 修完仍能在别的界面复现同样问题 → 没修到根因，退回重做。

## 3. 组件完成标准

- 无 open P0/P1；P2/P3 有明确处置（修/留档理由）。
- 契约相关项全部核对；截图证据齐全。
- 门禁全绿；`state.json` / `journal.md` 已更新并提交。
- 没有引入新的硬编码（计数不升）；可机械防护的问题已加 guard。

## 4. 失败与回滚

- 验证失败：先做一次针对性修复；仍失败则记录原因，`git restore` **仅限本次改动的文件**，`node ... abort --reason "..."`，结束。
- 不删除用户/历史改动；不 `checkout .`；不 `clean`。
- 运行中若发现主工作树被用户改动：无视，绝不跨树操作。
- 锁未释放（会话崩溃）：下次运行在 120 分钟后自动视为过期；不要手工删锁，除非确认无进程在跑。

## 5. 禁止清单（再强调）

- 禁止把修复做成“只在这一个弹窗生效”的坐标/颜色补丁。
- 禁止改 `PRODUCT` 行为来让视觉更顺（如上框时机、双击语义、auto-send）。
- 禁止新增第三方 UI/图标/动画依赖。
- 禁止删除/弱化测试与护栏。
- 禁止把未看图、未跑门禁的组件标 done。
- 禁止 push、禁止碰 main 工作树、禁止改 git 历史。

## 6. 截图与状态夹具

```bash
# 单个状态
node automation/ux-audit/scripts/capture.mjs --state settings-default --phase before

# 全量回归（无 pending 时）
node automation/ux-audit/scripts/capture.mjs --all --phase before
```

- 夹具在 `automation/ux-audit/states/fixtures.json`；`{{ROOT}}` 会被替换为工作树绝对路径。
- `window: {width,height}` 会用真实 `appwindow.slint` 生成一份仅改窗口尺寸的临时副本（`target/ux-audit/gen/`），用于 960×600 等尺寸审查；不维护第二份 UI 源码。
- `theme-light` 通过全局覆盖 `Palette.color-scheme` + `Theme.*` 模拟运行时浅色令牌。
- `settings-logs` / `settings-about` 刻意清空设置行模型，只为让日志/关于区落到首屏；这两个夹具里看到的布局高度异常要按真实设置模型复核，避免误报。
- 输出在 `target/ux-audit/shots/`（已忽略，不进库）。
- 新增弹窗状态时，必须同时设置 `*-open` 与 `*-shown`（若存在），否则入场 opacity 为 0。

## 7. 无 pending 时的回归巡检

1. `begin` 返回 `no_pending` 时不修改状态机。
2. 跑 `capture --all`，逐图对照契约；对发现的回归，用 `begin --component <最相关ID>` 进入正常流程记录/修复。
3. 若全量截图无问题：在 `journal.md` 追加一条“回归巡检通过（日期/commit/截图目录）”，**不提交空 commit**，结束。
4. 不要为了制造工作量而改样式；无事可做是合法结果。

## 8. 运行记录格式（state.mjs 自动写入 journal）

```markdown
## run-20261023-230000 · M3-C3 · done
- 组件：瓦片交互态/选中/徽标
- 修复：F-012 选中描边白 → Theme.accent（根因 token）
- 门禁：slint-check pass · cargo-check pass · deps-guard pass · ui-hygiene pass · visual pass
- 截图：target/ux-audit/shots/M3-C3/after/*.png
- 备注：hover 未无头截图，按状态矩阵核对
```

## 9. 首次运行提示

- 第一条工作单通常是 `M0-C1`（颜色 token）。先建立/补齐 token，再逐屏消费。
- `M0-C8`（UI 卫生 ratchet）已在初始化时完成；不要重复实现。
- 如果发现契约本身与现有产品决策冲突，按 §9 的契约修订流程单独处理，不要夹带在组件修复里。

## 10. 每小时定时任务（Qoder Automations）

- 任务：`ccdba4bb-1d50-4c0c-b93e-c25269d4b44c`「每小时 UX 视觉与交互审计（ux-audit）」，
  cron `0 * * * *`（Asia/Shanghai），独立会话，Full Access，`cwd` = 本 worktree。
- 提示词副本在 `automation/ux-audit/automation-prompt.md`；改提示词后必须用
  `qoder_cron action=update` 同步任务，否则下一次运行仍用旧提示词。
- 暂停/恢复：`qoder_cron action=disable|enable`；立即跑一次：`action=run`（会与进行中的构建争锁，
  建议先确认没有 `state.mjs status` 里的 current）。
- 单次运行可能超过 1 小时（首次全量编译），下一整点的运行会因锁直接退出——这是预期行为。
- 合并回主线由用户在主工作树执行 `git merge ux-audit`；自动化本身**永不 push、不合并 main 进 ux-audit 以外的分支**。
