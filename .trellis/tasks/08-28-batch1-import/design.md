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

## 5. 失败与边界

- 弹窗→子进程起功前失败（锁库/磁盘）：沿用 D36② 纪律（阶段一失败不得触发 thumbnails_generated 重载）。
- 散 .emo 与文件夹同名冲突：不处理（复制入库 uuid 命名，天然无冲突）。
- 「待分类」措辞 = `INBOX_CATEGORY` 常量复用（store::INBOX_CATEGORY）。
