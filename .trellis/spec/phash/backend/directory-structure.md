# Directory Structure — phash

## 布局

```
crates/phash/
├── Cargo.toml    # deps: image（仅 GrayImage 类型；不引哈希第三方库）
└── src/lib.rs    # perceptual_hash_gray + hamming_distance + 内联测试
```

## 模块组织规则

- 算法单文件：32×32 DCT-II → 左上 8×8 去 DC → 中位数阈值取位。
- 测试夹具是**程序化结构化图案**（`structured_pattern` 多频正弦叠加、`stripes` 条纹），禁止退化图——原因与教训见 quality-guidelines.md。
- 若 M4 后需要给 worker 复用，本 crate 保持零业务依赖即可直接进 worker 进程。
