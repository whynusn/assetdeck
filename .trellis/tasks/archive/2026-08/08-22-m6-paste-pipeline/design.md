# Design — M6 粘贴管线

## 边界

```
crates/platform/
├── Cargo.toml    # [dependencies] windows-sys(仅 win32 用到的 features);trait 部分零依赖
├── src/
│   ├── lib.rs        # 类型 + trait(ClipboardSink/FocusWatcher/KeyInjector)——零依赖、无 cfg
│   └── win32.rs      # #[cfg(windows)] mod:Win32Clipboard/Win32Focus/Win32Injector(真实实现)
└── tests/win32_manual.rs   # #[ignore] real_sendinput_into_notepad

crates/pipeline/
├── Cargo.toml    # deps: domain, platform
├── src/
│   ├── lib.rs        # PasteSession/PasteConfig/PasteOutcome/VirtualKey 常量
│   └── negotiate.rs  # 格式协商表(纯函数)
└── tests/pipeline_spec.rs  # 六个自动化测试(mock 全部平台依赖)
```

## 核心契约

### platform::lib(trait 层)

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardPayload {
    Files(Vec<PathBuf>),  // → CF_HDROP
    Png(Vec<u8>),         // → 注册格式 "PNG"
    Dib(Vec<u8>),         // → CF_DIB(仅接受上游已编码字节)
    Text(String),         // → CF_UNICODETEXT
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WindowHandle(pub isize);            // HWND 裸值

pub trait ClipboardSink { fn write(&mut self, payload: &ClipboardPayload) -> Result<(), PlatformError>; }
pub trait FocusWatcher { fn foreground(&self) -> WindowHandle; fn is_alive(&self, w: WindowHandle) -> bool; }
/// keys 为 VK 序列(按下+释放由调用方编排),实现按序 SendInput。
pub trait KeyInjector { fn inject(&mut self, keys: &[u16]) -> Result<(), PlatformError>; }
```

- `PlatformError`:Display+Error+From<io>,形态仿 StoreError;**不引 thiserror**(workspace 惯例:手写 Display match)。
- trait 文件零 cfg、零 win32 import(编译期可证的平台无关)。

### platform::win32(cfg(windows))

- `Win32Clipboard: ClipboardSink` — OpenClipboard(空剪贴板竞争容错重试一次)/EmptyClipboard/SetClipboardData/CloseClipboard;HDROP 构造 DROPFILES 头(双 NUL 结尾宽字符路径列表);失败路径必须 GlobalFree 防泄漏。
- `Win32Focus: FocusWatcher` — GetForegroundWindow / IsWindow。
- `Win32Injector: KeyInjector` — SendInput(KEYEVENTF 键盘扫描序列);VK 常量从 windows-sys 取,pipeline 不重复定义魔法数。

### pipeline::negotiate(纯函数表驱动)

```rust
pub enum AssetKind { Image, Video, Text }
pub struct AssetPayload<'a> { pub kind: AssetKind, pub png_bytes: &'a [u8], pub source_path: PathBuf, pub text: String }
pub enum TargetProfile { ImGeneric }              // v1 单一通用 profile,枚举留扩展位
/// 表:(kind, profile) → payload。Video→Files(源文件);Image→Png;Text→Text。
pub fn negotiate(req: &AssetPayload, profile: TargetProfile) -> Option<ClipboardPayload>;
```

- 返回 Option:无映射(未来未知组合)= None,调用方降级为 Files 或报不支持——测试覆盖 Image/Video/Text 三行。
- **Dib 行存在但不默认路由**(需上游提供已编码字节);注释说明 UI 进程不解码红线。

### pipeline::PasteSession

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PasteConfig { pub auto_send: bool }     // Default => false(红线快照测试)

pub enum PasteOutcome { Injected, CopiedOnly { reason: String }, Failed(String) }

pub struct PasteSession { config: PasteConfig, previous_foreground: Option<WindowHandle> }
impl PasteSession {
    pub fn new(config: PasteConfig) -> Self;
    pub fn begin_panel(&mut self, focus: &dyn FocusWatcher);      // 记录前一前台窗口(红线)
    pub fn previous_foreground(&self) -> Option<WindowHandle>;    // 测试钩子
    /// 顺序(操作日志断言):write_clipboard → is_alive(previous) → inject Ctrl+V → [auto_send] inject Enter
    pub fn paste(&mut self, req: &AssetPayload, deps: &mut dyn PipelineDeps) -> PasteOutcome;
}
```

- 注入序列:paste = `[VK_CONTROL down, 'V' down, 'V' up, VK_CONTROL up]`;auto_send 追加 `[VK_RETURN down, VK_RETURN up]`(VK_RETURN = 0x0D)。
- 焦点死/previous 无记录 → `CopiedOnly`,不再注入。
- `PipelineDeps` 组合三个 trait 的对象(dyn 字段),mock 实现记录 Op 日志。

### 操作日志(测试基建,mock 内部)

```rust
enum Op { WriteClipboard(ClipboardPayload), CheckAlive(WindowHandle), Inject(Vec<u16>) }
```
单一 `Vec<Op>` 共享给三个 mock(Arc<Mutex>),顺序断言即红线断言。

## 权衡记录

| 决策 | 备选 | 理由 |
|---|---|---|
| 手写 PlatformError | thiserror | workspace 既有惯例(store/library 均 thiserror-free?store 是手写 Display)保持一致 |
| dyn PipelineDeps | 泛型参数 | mock 注入简单,签名短 |
| VK 序列数组 | 封装 CtrlV/Enter 方法 | 测试能直接断言「序列不含 VK_RETURN」(M6 测试语义) |
| negotiate 返回 Option | Result | 「无映射」是合法查询结果而非错误 |

## 兼容与回滚

- 两 crate 均为占位替换,不动既有代码 → 回滚 = 还原 stub。
- win32 剪贴板实现复杂度高:若 gnu 下 DROPFILES 结构缺失等工具链问题,HDROP 可降级为「注册格式 + 自定义布局」并在 notes 记录(不阻塞六测,mock 层不受影响)。

## 测试钩子先例

- `previous_foreground()` 只读钩子;Op 日志本身即观测点(仿 library::set_paused 先例)。
