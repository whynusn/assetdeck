# Quality Guidelines — media

## 红线

1. **接口即契约**：本 crate 是「UI/编排层」与「解码实现」之间的防火墙。app-ui/ui-viewmodels 不得依赖任何解码 crate；media 的类型是它们能见到的全部媒体概念。
2. **依赖方向**：library → media ← worker（M4 迁移后）。media 不反向依赖任何业务 crate。
3. **可序列化**：跨进程 IPC 要求所有公共类型 `Serialize + Deserialize`。

## 验收关联

- M3 断言「UI 进程路径无解码调用」、M4 worker 实装都以此 crate 为界。
- cargo-deny bans + app-ui deps_guard 测试是编译期守卫，新增媒体依赖前先确认不破坏守卫。
