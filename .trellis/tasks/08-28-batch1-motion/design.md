# Design — 动效修复包 + 底边补齐

## 1. 弹层生命周期状态机（appwindow.slint）

三处旧弹层 + 新弹层统一收敛为同一组件内模式。每弹层：

```
property <bool> mounted    // if 块是否渲染（控制卸载时机）
property <bool> shown      // 动画目标态（opacity/translation 绑定源）
```

- **开**：`mounted = true`；`shown` 保持 false → 下一帧翻转。Slint 无 requestAnimationFrame → `SingleShot Timer(16ms) { shown = true }`（挂载事件 `init =>` 里启动 Timer；init 仍在首帧前，但只排 Timer 不改属性，回调必然跑在首帧之后——这正是根因修复点）。
- **关**：`shown = false` 播淡出；`Timer(150ms) { mounted = false }` 两段式卸载。
- **连点重入**：开→关→开 序列里，关段 Timer 触发前收到开：`mounted` 仍 true，直接 `shown = true`（取消挂起卸载——卸载 Timer 在开时 `restart`/stop）。关→开→关 同理只保一组 Timer 状态。
- 动画时长绑定 `root.animations-enabled ? 150ms : 0ms`（既有模式保留）；**开关关闭时两处 Timer 直接终态**（不开 Timer，mounted=shown 同步翻转），R4 达标。
- 落地形态：先抽 Slint 宏/内联复用块还是三处各自改写？Slint 无 mixin → **各弹层内联同构 5 行 + 一处 Rust 侧共享 Timer 封装不可行（Timer 归属组件实例）**，接受三处重复（同文件同模式，check 时对照一致性）；新弹层（菜单/操作条/归类弹窗/范围下拉）沿用同模式。
- 可测性：Slint 无单测框架 → 状态机走查 + 人工目测验收（自动化断言仅「首帧 opacity==0」若 `slint::` 测试路径可得则加，不强求）。

## 2. 瓦片淡入（thumbs.rs + appwindow.slint）

- `set_row_data` 定向更新（D43 渐进装载）换 thumb 时触发淡入：TileData 增 `thumb-fade: bool`（或复用现有字段语义）标记「本次写入为从无图→有图」。Slint 侧 `Rectangle.opacity` 动画：`animate opacity { duration: enabled ? 150ms : 0ms }`，thumb Image 包一层 opacity 绑定，tile 挂载时 `thumb-fade` false→true 播。
- **不随滚动重播**：淡入只在「该 tile 的 thumb 从无到有」发生时——判定在 build_rows/updates 路径（已有 `updates` 仅真实图像加载时置位，thumbs.rs:306-308），`set_row_data` 路径携带 fade 标志；整窗 `set_vec` 重建（切分类）时新建 tile 的 `thumb` 初值空、淡入在 update 时发生——与渐进装载天然吻合，无需额外防重播。
- 边界：负缓存（缺图）条目不触发淡入。

## 3. 底边几何稳定（thumbs.rs:176-204）

- `GridCtx` 增 `last_fill_y: Cell<f32>`；`fill_pass()` 开头读 `ui.get_content_y()`：
  ```
  stable = (this_y - last_fill_y).abs() < 0.5
  停表 = built.missing == 0 && stable
  ```
  缺图补完但几何未稳定（回弹中）→ 记 `last_fill_y = this_y`，照常排下轮 Timer。静止后最终一轮 = 「missing==0 && stable」空 pass 停表（多跑 ≤1 轮的开销，D54 已接受）。
- `sync()` 与 `schedule_fill()` 既有逻辑不动；fill 链只在 `missing>0 || !stable` 续跑——注意**稳定判据不能让无缺图的常态 sync 排 Timer**（常态 sync 不经过 fill_pass，停表只在 fill 内部，R7 达标）。
- 守卫测试：纯函数抽 `fill_should_stop(missing: usize, y_new: f32, y_last: f32) -> bool`（±0.5 阈值）单测表驱动（补完+稳定=停 / 补完+漂移=续 / 缺图+稳定=续）。

## 4. 与前三子的时序关系

motion 回改时各弹层代码已被前子扩展（dismiss 区、层级 z 序）——阶段顺序按 implement.md：先底边（独立），再旧弹层（冲突面最大，最后合）。
