# Design — 通用导入 + 导入归类弹窗

## 1. 收集层（tools/sample-library + app-ui 壳层）

### 1.1 统一入口的输入模型

- 新增 `ImportSource` 分类（收集层概念，VM/CLI 共享语义）：
  - `LooseFiles(Vec<PathBuf>)` — 散素材/散 .emo 直接路径
  - `Folder(PathBuf)` — 文件夹树（含千牛结构目录判定）
  - `Package(PathBuf)` — .emo 文件
- 混选对话框返回的 `Vec<PathBuf>` 按「是目录？是 .emo？其余=散文件」三分；同一批可并存多来源 → 弹窗按来源分组给选项行。

### 1.2 CLI 扩展（sample-library）

- `import_package(inbox, out, mode)` 现为单目录入口；新增 `--import-paths <file>`：file 每行一条来源（前缀 `f:`=散文件、`d:`=目录、`p:`=.emo），逐条走 `PackageRegistry`（D24 首命中者胜）+ 显式 category 参数 `--category-override <name>`（存在时 RuleChain explicit 置 Some，覆盖包内规则）/ `--force-inbox` / 缺省=按来源规则（= 现行为，记住路径复用）。
- 千牛结构目录判定 = ParentDirectoryRule 已有逻辑（D29），无需新代码。
- **修订（阶段 1 实施时定案）**：归类决策不能是全局旗标——混选各组分属不同
  决策，全局 `--category-override` 会把 .emo 组的包内分类一并覆盖。改为逐行
  指令：`<kind>	<mode>	<path>`，mode ∈ `auto` | `inbox` | `category:<名称>`；
  散文件不走注册表（DirectoryReader 只认目录），按 media 注册表判扩展名直接
  入列，不支持 = 静默跳过（R4）。预扫描出 `--probe-categories <path>` 分支，
  stdout 一行 `PROBE<HT>categories=<n|none>`（zip 只读中央目录不解压）。

## 2. 归类弹窗 VM（crates/ui-viewmodels，纯函数）

- `classify.rs` 新模块：
  ```
  enum ClassifyMode { PackageInternal, ByFolderName, Unified(category), Inbox }
  struct SourceGroup { source: SourceKind, default: ClassifyMode, options: Vec<ClassifyOption> }
  fn plan_groups(paths: &[ImportSource]) -> Vec<SourceGroup>   // 穷举测试映射 D50 表
  ```
- 「含 N 个分类」：`SourceKind::PackageEmo` → EmoReader 读清单计数（不拷文件不解码）；`Folder` → 直接子目录数（>1 时按文件夹名归类才有分叉，标注 N=子目录数）。预扫描在起弹窗前同步跑（目录 stat 级，毫秒）。
- 记住方式：`AppSettings` 新字段 `import_classify_memory: Vec<RememberedClassify>`（TOML 表：来源类型 → 方式(+分类名)）；`describe()` 增一行设置「导入时询问归类」toggle（关闭=全部套用记忆，缺失来源=默认方式）——D28 机制，不发明新模式。

## 3. 弹窗 UI（appwindow.slint）

- `ClassifyDialog` 组件：每来源一组（来源摘要行 + RadioGroup 选项行 + 条件性「统一归入 ▼ 可输入下拉」——Slint ComboBox 不吃自由文本，用 ComboBox+LineEdit 组合或 PopupMenu+新建行，spike 定）；底部「导入」/「取消」+「记住我的选择」勾选。
- 动效：入场 = 挂载后 16ms SingleShot Timer 翻 `shown`（首帧 opacity 0 渲染）；出场 = 两段式（先翻出播 150ms，Timer 到点真销毁）；`animations-enabled ? … : 0ms` 钳制——本任务先行，motion 子任务回改旧三处。
- 数据通道：`in property <[SourceGroupData]>` + `callback classify-confirmed(string json)`（或结构化回传每组选择）；确认后才 `spawn_import_pipeline`。

## 4. 拖拽导入（D49 spike → 两路径）

- **Spike（阶段 0）**：Slint 1.17 `Window`/`Component` 是否有 drop 事件或 `drag-drop` 能力（查 i-slint-core 与 slint 文档 1.17 变更史）。
- 有 → slint 侧接路径列表，转 `plan_groups`。
- 无 → platform 层兜底：`RegisterDragDrop`（`IDropTarget`）装在主窗口 HWND（`win32_runtime_deps()` 装配，D16）；`DragQueryFileW` 取路径 → channel 送 UI 事件循环（`slint::invoke_from_event_loop`）。`DropTarget` 须处理 `WM_DROPFILES`/OleSetClipboard 竞争与 `DND_OLE_NOT_INITED`——窗口过程由 Slint 持有，兜底采用**子类化（SetWindowSubclass）**还是 `IDropTarget` 注册，spike 时一并定，落 `platform::win32::dragdrop`，trait `FileDropSink` 进 lib 层供装配。
- 风险登记：Slint 内部可能已注册自有 drop 处理（文本 drop），实现时先验证共存。

### 4.1 Spike S1 结论（2026-08-28，源码查证 1.17.1，走 4.1b 兜底路线）

**判定：Slint 1.17.1 不支持 OS 文件拖入，`DropArea` 仅覆盖应用内 DnD。**

证据链（安装源即 ground truth）：
1. `i-slint-core` 有 `DragArea`/`DropArea` 元素与 `DataTransfer`（items/drag_n_drop.rs），
   但核心 input.rs 的 drop 事件只有 `DragMove/Drop`（源 = 本进程 DragArea）；
2. `i-slint-backend-winit` 全文 grep `DroppedFile` **零命中**——winit 0.30 已转发该事件
   （OS 文件拖入），Slint 的 match 臂 `_ => {}` 直接吞掉；
3. `RegisterDragDrop`/`OleInitialize`/`DragAcceptFiles` 在 Slint 任何 crate 零命中，
   **零共存冲突**（风险登记的担忧解除：Slint 根本没注册 drop 处理）。

兜底路线定案（4.1b）：
- `IDropTarget` via `RegisterDragDrop`（**不是** `DragAcceptFiles`/WM_DROPFILES——后者只给
  文件名、无 OLE 反馈，拖拽时光标不显示「复制」且无法动态 Accept/Drop 决策；IDropTarget
  可经 `GetAncestor(GA_ROOT)` 拿 winit HWND 注册，无需子类化、无需碰 Slint 的窗口过程）。
- UI 线程已是 COM STA（win32.rs `ensure_com_initialized`，OleInitialize 在其上补一次即可）；
  IDropTarget 回调天然在消息循环线程派发 → drop 路径经 mpsc/channel 送 `invoke_from_event_loop`。
- 释放：`CoTaskMemFree` 取 `HDROP`（`DragQueryFileW` 计数+取名）+ `ReleaseStgMedium`。
- `FileDropSink` trait 进 lib 层（装配签名进 `win32_runtime_deps()`，D16 边界不变）。
- 悬停视觉反馈（可选，若工期允许）：DropArea 的 can-drop 回调收不到 OS 悬停，改用
  `drag_enter/leave` 计数经属性透传一个高亮 Rectangle（简单态：整窗描边）。

### 4.2 Spike S2 结论（ComboBox 自由文本）

Slint 1.17 `ComboBox` 只从 model 选择、无自由文本输入。定案组合 =
**行内 LineEdit（输入即新建候选）+ 下方分类下拉**（下拉选中回填 LineEdit；
「统一归入」以 LineEdit 现值为准）。比 PopupMenu 组合少一层浮层管理，
且天然支持「输入新名即新建」的 R7 语义。

## 5. 失败与边界

- 弹窗→子进程起功前失败（锁库/磁盘）：沿用 D36② 纪律（阶段一失败不得触发 thumbnails_generated 重载）。
- 散 .emo 与文件夹同名冲突：不处理（复制入库 uuid 命名，天然无冲突）。
- 「待分类」措辞 = `INBOX_CATEGORY` 常量复用（store::INBOX_CATEGORY）。
