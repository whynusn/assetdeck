//! D46/D48 库写子命令族：`sample-library --cmd trash|restore|purge|empty-trash|rename|move-category …`。
//!
//! 为什么是子命令而不是库内直接函数：UI 进程直写 meta.db 会制造第二写者
//! （库写单入口纪律，同导入管线走子进程的理由）。壳层 ChildTaskRunner
//! 驱动（D33），stdout 逐行 `PROGRESS\t<done>\t<total>`，收尾 `done:` 汇总行，
//! 失败走非零退出码 + stderr（协议注释见 task_runner.rs 头）。

use std::path::Path;

use library::Library;

pub(crate) struct LibraryCmd {
    pub action: String,
    pub library: std::path::PathBuf,
    pub uuids: Vec<String>,
    pub value: Option<String>,
}

/// `--cmd <action> --library <root> [--uuid <u>]… [--value <v>]`
/// 返回 Err 表示参数错误（调用方出 usage）；None 表示不是 --cmd 调用。
pub(crate) fn parse_library_cmd(args: &[String]) -> Result<Option<LibraryCmd>, String> {
    let Some(pos) = args.iter().position(|a| a == "--cmd") else {
        return Ok(None);
    };
    let action = args.get(pos + 1).ok_or("--cmd 缺少动作名")?.clone();
    let mut library = None;
    let mut uuids = Vec::new();
    let mut value = None;
    let mut it = args.iter().skip(pos + 2);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--library" => {
                library =
                    Some(Path::new(it.next().ok_or("--library 缺少值")?.as_str()).to_path_buf())
            }
            "--uuid" => uuids.push(it.next().ok_or("--uuid 缺少值")?.clone()),
            "--value" => value = Some(it.next().ok_or("--value 缺少值")?.clone()),
            other => return Err(format!("--cmd 不识别的参数: {other}")),
        }
    }
    Ok(Some(LibraryCmd {
        action,
        library: library.ok_or("--cmd 需要 --library <root>")?,
        uuids,
        value,
    }))
}

pub(crate) fn execute(cmd: &LibraryCmd) -> Result<(), String> {
    let lib = Library::open(&cmd.library).map_err(|e| e.to_string())?;
    let total = cmd.uuids.len();
    let refs: Vec<&str> = cmd.uuids.iter().map(String::as_str).collect();
    let done: usize = match cmd.action.as_str() {
        "trash" => lib.move_to_trash(&refs).map_err(|e| e.to_string())?,
        "restore" => lib.restore_from_trash(&refs).map_err(|e| e.to_string())?,
        "purge" => lib.purge(&refs).map_err(|e| e.to_string())?,
        "empty-trash" => lib.empty_trash().map_err(|e| e.to_string())?,
        "rename" => {
            let name = cmd.value.as_deref().ok_or("rename 需要 --value <新名>")?;
            if name.trim().is_empty() || name.contains('/') || name.contains('\\') {
                return Err("新名不能为空或含路径分隔符".to_string());
            }
            let mut n = 0usize;
            for uuid in &refs {
                if lib.store().rename_asset(uuid, name).map_err(|e| e.to_string())? {
                    n += 1;
                }
                println!("PROGRESS\t{}\t{total}", n.min(total));
            }
            n
        }
        "move-category" => {
            let raw = cmd.value.as_deref().ok_or("move-category 需要 --value <分类名>")?;
            // 空值 = 归待分类（category NULL，读取侧 COALESCE 显示为待分类）。
            let category = if raw.is_empty() { None } else { Some(raw) };
            let mut n = 0usize;
            for uuid in &refs {
                if lib.store().set_category(uuid, category).map_err(|e| e.to_string())? {
                    n += 1;
                }
                println!("PROGRESS\t{}\t{total}", n.min(total));
            }
            n
        }
        other => {
            return Err(format!(
                "未知 --cmd {other}（可用：trash | restore | purge | empty-trash | rename | move-category）"
            ))
        }
    };
    if !matches!(cmd.action.as_str(), "rename" | "move-category") {
        // 批量动作无逐件进度，收尾报满（协议要求 PROGRESS 行，壳层进度条收口）。
        println!("PROGRESS\t{total}\t{total}");
    }
    println!(
        "done: affected={done} requested={total} root={}",
        cmd.library.display()
    );
    Ok(())
}
