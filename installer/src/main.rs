#![windows_subsystem = "windows"]

use std::path::{Path, PathBuf};
use std::process::Command;

use flate2::read::GzDecoder;
use tar::Archive;

const PAYLOAD: &[u8] = include_bytes!("../../dist.tar.gz");
const APP_NAME: &str = "素材管理器";
const INSTALL_DIR_NAME: &str = "素材管理器";
const MAIN_EXE: &str = "asset-manager.exe";

fn main() {
    let mut silent = false;
    let mut with_shortcuts = true;
    let mut install_dir = install_dir();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--silent" | "-y" => silent = true,
            "--install-dir" => {
                if let Some(dir) = args.next() {
                    install_dir = PathBuf::from(dir);
                }
            }
            "--no-shortcuts" => with_shortcuts = false,
            _ => {
                if let Some(dir) = arg.strip_prefix("--install-dir=") {
                    install_dir = PathBuf::from(dir);
                }
            }
        }
    }

    if !silent {
        let confirmed = confirm_install(&install_dir);
        if !confirmed {
            return;
        }
    }

    if let Err(err) = install(&install_dir) {
        let _ = message_box(
            "安装失败",
            &format!("安装失败：\n{}\n\n请关闭正在运行的素材管理器后重试。", err),
            MB_ICONERROR | MB_OK,
        );
        std::process::exit(1);
    }

    if with_shortcuts {
        let _ = create_shortcuts(&install_dir);
    }
    let _ = launch_app(&install_dir);

    if !silent {
        let _ = message_box(
            "安装完成",
            &format!("{} 已安装到：\n{}\n\n正在启动程序。", APP_NAME, install_dir.display()),
            MB_ICONINFORMATION | MB_OK,
        );
    }
}

fn install_dir() -> PathBuf {
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        PathBuf::from(local).join("Programs").join(INSTALL_DIR_NAME)
    } else if let Some(profile) = std::env::var_os("USERPROFILE") {
        PathBuf::from(profile)
            .join("AppData")
            .join("Local")
            .join("Programs")
            .join(INSTALL_DIR_NAME)
    } else {
        PathBuf::from("素材管理器")
    }
}

fn confirm_install(dir: &Path) -> bool {
    let msg = format!(
        "即将把 {} 安装到当前用户目录：\n{}\n\n安装不需要管理员权限。是否继续？",
        APP_NAME,
        dir.display()
    );
    let choice = message_box("安装", &msg, MB_ICONQUESTION | MB_YESNO);
    choice == IDYES
}

fn install(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("创建安装目录失败: {}", e))?;

    let decoder = GzDecoder::new(PAYLOAD);
    let mut archive = Archive::new(decoder);
    archive
        .unpack(dir)
        .map_err(|e| format!("解压文件失败: {}", e))?;

    let main_exe = dir.join(MAIN_EXE);
    if !main_exe.is_file() {
        return Err("安装包中缺少 asset-manager.exe".to_string());
    }

    Ok(())
}

fn create_shortcuts(install_dir: &Path) -> Result<(), String> {
    let desktop = PathBuf::from(powershell_known_folder("Desktop")?);
    let programs = PathBuf::from(powershell_known_folder("Programs")?);

    let start_menu_dir = programs.join(INSTALL_DIR_NAME);
    std::fs::create_dir_all(&start_menu_dir).map_err(|e| e.to_string())?;

    let exe = install_dir.join(MAIN_EXE);
    let working_dir = install_dir.to_string_lossy().into_owned();

    create_shortcut(&desktop, APP_NAME, &exe, &working_dir)?;
    create_shortcut(&start_menu_dir, APP_NAME, &exe, &working_dir)?;

    Ok(())
}

fn powershell_known_folder(kind: &str) -> Result<String, String> {
    let script = format!(
        "[Environment]::GetFolderPath('{}')",
        kind
    );
    let output = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", &script])
        .output()
        .map_err(|e| format!("无法调用 PowerShell 获取 {} 目录: {}", kind, e))?;
    if !output.status.success() {
        return Err(format!(
            "PowerShell 获取 {} 目录失败: {}",
            kind,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() {
        return Err(format!("无法确定 {} 目录", kind));
    }
    Ok(path)
}

fn create_shortcut(folder: &Path, name: &str, target: &Path, working_dir: &str) -> Result<(), String> {
    let shortcut_path = folder.join(format!("{}.lnk", name));
    let script = format!(
        "$ws = New-Object -ComObject WScript.Shell; \
         $s = $ws.CreateShortcut('{}'); \
         $s.TargetPath = '{}'; \
         $s.WorkingDirectory = '{}'; \
         $s.IconLocation = '{}'; \
         $s.Save()",
        shortcut_path.display().to_string().replace('\'', "''"),
        target.display().to_string().replace('\'', "''"),
        working_dir.replace('\'', "''"),
        target.display().to_string().replace('\'', "''"),
    );
    let output = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", &script])
        .output()
        .map_err(|e| format!("无法创建快捷方式 {}: {}", shortcut_path.display(), e))?;
    if !output.status.success() {
        return Err(format!(
            "创建快捷方式 {} 失败: {}",
            shortcut_path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

fn launch_app(install_dir: &Path) -> Result<(), String> {
    let exe = install_dir.join(MAIN_EXE);
    Command::new(&exe)
        .current_dir(install_dir)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("启动 {} 失败: {}", exe.display(), e))
}

#[cfg(windows)]
fn message_box(title: &str, text: &str, kind: u32) -> i32 {
    use std::iter::once;
    use windows_sys::Win32::UI::WindowsAndMessaging::MessageBoxW;

    let title: Vec<u16> = title.encode_utf16().chain(once(0)).collect();
    let text: Vec<u16> = text.encode_utf16().chain(once(0)).collect();
    unsafe { MessageBoxW(std::ptr::null_mut(), text.as_ptr(), title.as_ptr(), kind) }
}

#[cfg(not(windows))]
fn message_box(_title: &str, text: &str, _kind: u32) -> i32 {
    eprintln!("{}", text);
    0
}

const MB_OK: u32 = 0x00000000;
const MB_YESNO: u32 = 0x00000004;
const MB_ICONQUESTION: u32 = 0x00000020;
const MB_ICONINFORMATION: u32 = 0x00000040;
const MB_ICONERROR: u32 = 0x00000010;
const IDYES: i32 = 6;
