# Database Guidelines — targets

## 零持久化边界

- 本 crate 不接触数据库、文件系统、注册表、环境变量或应用配置目录。
- `profiles.builtin.toml` 与 `profiles.user.toml` 的路径选择、读取、原子保存和升级迁移归二进制装配/应用配置层。
- `load_profiles(builtin: &str, user: Option<&str>)` 是唯一配置入口形态；测试直接传字符串。

## 数据源顺序

```text
builtin TOML 字符串
    + user TOML 字符串（同 id 字段级覆盖）
    -> ProfileSet
```

升级不得覆盖用户文件。当前 `app-ui` 运行时仍传 `None`，因此用户 profile 的读取和持久化尚未交付。

## 稳定目标册

若后续增加账号/会话级稳定目标存储，持久化记录必须保存稳定实例身份，不得保存 HWND 作为主键。HWND 只能作为进程生命周期内的缓存绑定。
