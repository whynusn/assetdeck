# Directory Structure — media

## 布局

```
crates/media/
└── src/lib.rs    # 仅接口定义：MediaJob 相关类型 / trait
```

## 定位约束

- **本 crate 只放接口**（任务类型、dispatcher 抽象的媒体侧定义），实现在 worker crate——TDD_PLAN 第二节依赖图的规定。
- 注意：当前 `MediaJob`/`MediaDispatcher` 实际定义在 library/src/lib.rs（M3 时序产物）；M4 接入 worker 时应将共享类型迁移到 media crate，library 与 worker 都依赖它。迁移时保持 library 测试全绿。
- **禁止**在本 crate 写解码代码或引入 image/ffmpeg 类依赖。
