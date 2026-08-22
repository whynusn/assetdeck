# Error Handling — phash

- 纯函数无错误路径：`perceptual_hash_gray(&GrayImage)` 输入已由调用方保证 ≥32×32（library 层解码后传入）。
- 输入尺寸不足属程序员错误 → 允许 panic（get_pixel 越界）；如需防御，在 library 编排层先做缩放，不在本 crate 加 Result。
- `hamming_distance(a, b)` 全域可逆运算，永不出错。

**禁止**：引入 thiserror/anyhow——本 crate 无错误可表达。
