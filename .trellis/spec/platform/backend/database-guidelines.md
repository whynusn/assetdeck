# Database Guidelines — platform

- 不接触数据库。剪贴板格式常量（CF_HDROP/PNG/DIB/CF_UNICODETEXT）属于本 crate 的 win32 实现细节，pipeline 通过 trait 抽象使用。
