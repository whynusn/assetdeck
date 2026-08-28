#![windows_subsystem = "windows"]

use std::path::{Path, PathBuf};
use std::process::Command;

use flate2::read::GzDecoder;
use tar::Archive;

#[cfg(windows)]
use windows::core::{GUID, Interface, PCWSTR};
#[cfg(windows)]
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED, IPersistFile,
};
#[cfg(windows)]
use windows::Win32::UI::Shell::{
    FOLDERID_Desktop, FOLDERID_Programs, IShellLinkW, KNOWN_FOLDER_FLAG, SHGetKnownFolderPath,
};

// windows 0.62 起不再预生成 CLSID 常量；Shell Link 对象的 CLSID 是稳定 COM 契约。
const CLSID_SHELL_LINK: GUID = GUID::from_u128(0x00021401_0000_0000_C000_000000000046);

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

#[cfg(windows)]
fn create_shortcuts(install_dir: &Path) -> Result<(), String> {
    let desktop = PathBuf::from(known_folder(&FOLDERID_Desktop, "桌面")?);
    let programs = PathBuf::from(known_folder(&FOLDERID_Programs, "开始菜单")?);

    let start_menu_dir = programs.join(INSTALL_DIR_NAME);
    std::fs::create_dir_all(&start_menu_dir).map_err(|e| e.to_string())?;

    let exe = install_dir.join(MAIN_EXE);
    let working_dir = install_dir.to_string_lossy().into_owned();

    create_shortcut(&desktop, APP_NAME, &exe, &working_dir)?;
    create_shortcut(&start_menu_dir, APP_NAME, &exe, &working_dir)?;

    Ok(())
}

#[cfg(not(windows))]
fn create_shortcuts(_install_dir: &Path) -> Result<(), String> {
    Err("快捷方式创建仅支持 Windows".to_string())
}

/// 已知目录（桌面/开始菜单程序组）：SHGetKnownFolderPath 原生获取。
#[cfg(windows)]
fn known_folder(folder: &GUID, label: &str) -> Result<String, String> {
    unsafe {
        let path = SHGetKnownFolderPath(folder, KNOWN_FOLDER_FLAG(0), None)
            .map_err(|e| format!("获取{label}目录失败: {e}"))?;
        let s = path
            .to_string()
            .map_err(|e| format!("{label}目录路径编码异常: {e}"))?;
        CoTaskMemFree(Some(path.as_ptr().cast()));
        if s.is_empty() {
            return Err(format!("无法确定{label}目录"));
        }
        Ok(s)
    }
}

/// .lnk 快捷方式：IShellLinkW + IPersistFile 原生 COM，不再经 PowerShell。
#[cfg(windows)]
fn create_shortcut(folder: &Path, name: &str, target: &Path, working_dir: &str) -> Result<(), String> {
    let shortcut_path = folder.join(format!("{}.lnk", name));
    let to_wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() };
    let target16 = to_wide(&target.display().to_string());
    let work16 = to_wide(working_dir);
    let link16 = to_wide(&shortcut_path.display().to_string());

    unsafe {
        let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        if hr.is_err() {
            return Err(format!("COM 初始化失败: {hr:?}"));
        }
        let result = (|| -> Result<(), windows::core::Error> {
            let link: IShellLinkW = CoCreateInstance(&CLSID_SHELL_LINK, None, CLSCTX_INPROC_SERVER)?;
            link.SetPath(PCWSTR(target16.as_ptr()))?;
            link.SetWorkingDirectory(PCWSTR(work16.as_ptr()))?;
            link.SetIconLocation(PCWSTR(target16.as_ptr()), 0)?;
            let persist: IPersistFile = link.cast()?;
            persist.Save(PCWSTR(link16.as_ptr()), false)?;
            Ok(())
        })();
        CoUninitialize();
        result
            .map_err(|e| format!("创建快捷方式 {} 失败: {e}", shortcut_path.display()))?;
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
