//! 视频（及其他非 image-crate 格式）抽帧：Windows Shell 缩略图工厂。
//!
//! 为什么不是 ffmpeg：机器上没有 ffmpeg，而给产品捆一个外部二进制会把
//! 「用户能不能看清素材」绑到工具链是否就位。`IShellItemImageFactory` 是系统
//! 自带能力，顺带覆盖所有注册了缩略图处理器的格式（mp4/mov/pdf/office…）。
//! 代价：拿到的是系统给出的代表帧，无法指定时间点——对「辨识素材」这个目标够用。
//!
//! 位置纪律（D11）：仅在 decode-worker 子进程内调用，宿主 UI 进程永不触碰。

use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows::core::PCWSTR;
use windows::Win32::Foundation::SIZE;
use windows::Win32::Graphics::Gdi::{
    DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO, BITMAPINFOHEADER,
    BI_RGB, DIB_RGB_COLORS, HBITMAP, HGDIOBJ,
};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::UI::Shell::{
    IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_BIGGERSIZEOK, SIIGBF_THUMBNAILONLY,
};

/// 解码出的一帧：RGBA8 连续像素 + 尺寸。
pub struct Frame {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// 进程级 COM 初始化。重复调用返回 S_FALSE，无害；失败也不致命（后续调用自会报错）。
pub fn init_com() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
}

/// 向 Shell 索要一帧代表画面。
///
/// 返回尺寸是 Shell 给出的帧尺寸而非容器编码分辨率；Shell 保持原始比例，
/// 因此瀑布流所需的宽高比是准确的。
pub fn extract_frame(path: &Path, max_edge: u32) -> Result<Frame, String> {
    if !path.is_file() {
        return Err(format!("文件不存在 {path:?}"));
    }
    // Shell 只认绝对解析名；相对路径会按 shell 自己的当前目录解析。
    let absolute = std::path::absolute(path).map_err(|e| format!("绝对化 {path:?} 失败: {e}"))?;
    // 路径可能含非 UTF-8 可表示的字符，走 OsStr 的原生 UTF-16 视图而非 to_string_lossy。
    let wide: Vec<u16> = absolute
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let edge = max_edge.clamp(32, 2048) as i32;

    unsafe {
        let factory: IShellItemImageFactory =
            SHCreateItemFromParsingName(PCWSTR(wide.as_ptr()), None)
                .map_err(|e| format!("Shell 无法解析 {absolute:?}: {e}"))?;
        // THUMBNAILONLY：只要真实画面。缺了它，Shell 会拿「文件类型通用图标」
        // 充数——损坏或不可解码的素材会静默得到一张 PNG 图标，看起来像成功。
        // BIGGERSIZEOK：宁可接受比请求略大的缓存帧，也不要 Shell 直接放弃。
        let bitmap = factory
            .GetImage(
                SIZE { cx: edge, cy: edge },
                SIIGBF_BIGGERSIZEOK | SIIGBF_THUMBNAILONLY,
            )
            .map_err(|e| format!("Shell 未能给出缩略图 {absolute:?}: {e}"))?;
        let frame = copy_rgba(bitmap);
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        frame
    }
}

/// HBITMAP → RGBA8。以 32bpp、负高度（自上而下）取回 BGRA 后换序。
unsafe fn copy_rgba(bitmap: HBITMAP) -> Result<Frame, String> {
    let mut header = BITMAP::default();
    let read = unsafe {
        GetObjectW(
            HGDIOBJ(bitmap.0),
            std::mem::size_of::<BITMAP>() as i32,
            Some(&mut header as *mut BITMAP as *mut c_void),
        )
    };
    if read == 0 || header.bmWidth <= 0 || header.bmHeight == 0 {
        return Err("读取位图头失败".to_string());
    }
    let width = header.bmWidth as u32;
    let height = header.bmHeight.unsigned_abs();

    let screen = unsafe { GetDC(None) };
    if screen.is_invalid() {
        return Err("GetDC 失败".to_string());
    }

    let mut info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: header.bmWidth,
            // 负高度 = 自上而下扫描，省一次行翻转。
            biHeight: -(height as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut buffer = vec![0u8; width as usize * height as usize * 4];
    let lines = unsafe {
        GetDIBits(
            screen,
            bitmap,
            0,
            height,
            Some(buffer.as_mut_ptr() as *mut c_void),
            &mut info,
            DIB_RGB_COLORS,
        )
    };
    unsafe {
        ReleaseDC(None, screen);
    }
    if lines == 0 {
        return Err("GetDIBits 未取到扫描行".to_string());
    }

    // BGRA → RGBA，并强制不透明：Shell 给视频帧的 alpha 常为 0（未初始化），
    // 直接落盘会得到一张全透明 PNG——与「没有预览」在观感上毫无区别。
    let mut i = 0;
    while i + 3 < buffer.len() {
        buffer.swap(i, i + 2);
        buffer[i + 3] = 0xFF;
        i += 4;
    }
    Ok(Frame {
        rgba: buffer,
        width,
        height,
    })
}
