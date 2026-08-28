//! `--import-paths` 端到端（D49/D50 阶段 1.1 红灯的进程边界版）：
//!
//! 为什么是集成测试而不是 bin 内联：run_import 返回后库后台线程仍持有
//! meta.db 句柄（Windows 共享冲突，in-process 重开必挂）；且真协议走的就是
//! 子进程边界——起真 exe、读退出码与 stdout，再用 rusqlite 直查落库结果。
//!
//! 用例对应 implement.md 1.1：混选 f:+d: → 包内分类 + 散文件 override；
//! category 胜 groupName；force-inbox；取消路径（空清单）零文件进库。

use std::path::{Path, PathBuf};
use std::process::Command;

use image::DynamicImage;

fn temp_root(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!(
        "import_paths_e2e_{tag}_{}_{nanos}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn write_png(path: &Path) {
    let img = image::RgbImage::from_fn(8, 8, |_x, _y| image::Rgb([180, 180, 180]));
    DynamicImage::ImageRgb8(img)
        .save(path)
        .expect("写测试图失败");
}

/// 千牛结构目录：容器/组A/{EmotionConfig.json, a.jpg}。读取器对源根自身的
/// config 不走千牛分支（dir != root 判定），fixture 必须包一层容器。
fn make_qianniu_container(root: &Path) -> PathBuf {
    let group = root.join("容器").join("组A");
    std::fs::create_dir_all(&group).unwrap();
    std::fs::write(
        group.join("EmotionConfig.json"),
        r#"[{"originalFile":"a.jpg","groupName":"表情组"}]"#,
    )
    .unwrap();
    write_png(&group.join("a.jpg"));
    root.join("容器")
}

/// 起真子进程执行 --import-paths，断言成功并返回 stdout 全文。
fn run_import_cli(paths_text: &str, library: &Path) -> String {
    let list = library.parent().unwrap().join("paths.txt");
    std::fs::write(&list, paths_text).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sample-library"))
        .args([
            "--import-paths",
            &list.display().to_string(),
            "--library",
            &library.display().to_string(),
            "--mode",
            "fast",
        ])
        .output()
        .expect("起 sample-library 子进程失败");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "子进程应成功，stderr={}，stdout={stdout}",
        String::from_utf8_lossy(&output.stderr)
    );
    stdout
}

fn categories(lib: &Path) -> Vec<String> {
    let mut cats: Vec<String> = {
        let conn = rusqlite::Connection::open(lib.join("meta.db")).unwrap();
        let mut stmt = conn
            .prepare("SELECT COALESCE(category, '待分类') AS c FROM assets GROUP BY c")
            .unwrap();
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        rows
    };
    cats.sort();
    cats
}

#[test]
fn mixed_sources_package_rules_and_loose_override() {
    let root = temp_root("mixed");
    let qn = make_qianniu_container(&root);
    write_png(&root.join("loose.png"));
    let lib = root.join("library");

    run_import_cli(
        &format!(
            "d\tauto\t{}\nf\tcategory:相册\t{}\n",
            qn.display(),
            root.join("loose.png").display()
        ),
        &lib,
    );

    assert_eq!(
        categories(&lib),
        vec!["相册".to_string(), "表情组".to_string()],
        "包内分类与散文件 override 应各行其道"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn category_override_beats_group_name() {
    let root = temp_root("override");
    let qn = make_qianniu_container(&root);
    let lib = root.join("library");

    run_import_cli(&format!("d\tcategory:统一\t{}\n", qn.display()), &lib);

    assert_eq!(
        categories(&lib),
        vec!["统一".to_string()],
        "explicit 统一归入应胜过包内 groupName（RuleChain 既有语义）"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn force_inbox_lands_in_inbox_category() {
    let root = temp_root("inbox");
    let qn = make_qianniu_container(&root);
    let lib = root.join("library");

    run_import_cli(&format!("d\tinbox\t{}\n", qn.display()), &lib);

    assert_eq!(
        categories(&lib),
        vec!["待分类".to_string()],
        "force-inbox 应落待分类（category 置 None → 落库归一）"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn empty_paths_writes_nothing() {
    let root = temp_root("empty");
    let lib = root.join("library");
    run_import_cli("", &lib);
    assert!(
        !lib.join("meta.db").exists(),
        "空清单（取消路径）不得创建 meta.db"
    );
    std::fs::remove_dir_all(root).unwrap();
}

/// 冒烟对照：旧两位置参数形式不被本任务破坏（D35/D36 入口兼容）。
#[test]
fn legacy_positional_form_still_imports() {
    let root = temp_root("legacy");
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    write_png(&src.join("solo.png"));
    let lib = root.join("library");

    let output = Command::new(env!("CARGO_BIN_EXE_sample-library"))
        .args([
            &src.display().to_string(),
            &lib.display().to_string(),
            "--mode",
            "fast",
        ])
        .output()
        .expect("起 sample-library 子进程失败");
    assert!(output.status.success());
    assert_eq!(categories(&lib), vec!["待分类".to_string()]);
    std::fs::remove_dir_all(root).unwrap();
}
