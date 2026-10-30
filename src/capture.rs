//! Win32 GDI 截屏:BitBlt + CAPTUREBLT 可以连同分层窗口(我们的墨迹层)一起抓下来,
//! 所以"存图/复制"直接抓当前屏幕就是标注后的效果,不需要额外合成。
//!
//! 坐标约定:截屏坐标一律是**虚拟桌面物理像素**——主屏左上角为 (0,0),
//! 位于主屏左侧/上方的显示器给出负坐标。这样副屏也能正确抓取,
//! GetDC(None) 拿到的屏幕 DC 正是按这套坐标工作的。
//!
//! 标注是"单屏"的:调用方先用 [`monitor_from_point`] 找到按钮所在的显示器,
//! 再把这块显示器的矩形交给 [`capture_region_px`] 整块抓取。

use windows::core::BOOL;
use windows::Win32::Foundation::{LPARAM, POINT, RECT};
use windows::Win32::Graphics::Dwm::DwmFlush;
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject,
    EnumDisplayMonitors, GetDC, GetDIBits, GetMonitorInfoW, MonitorFromPoint, ReleaseDC,
    SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CAPTUREBLT, DIB_RGB_COLORS, HDC, HMONITOR,
    MONITORINFO, MONITOR_DEFAULTTONEAREST, SRCCOPY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetSystemMetrics, SM_CXSCREEN, SM_CXVIRTUALSCREEN, SM_CYSCREEN,
    SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

use crate::i18n::Lang;

pub struct Shot {
    pub width: i32,
    pub height: i32,
    pub rgba: Vec<u8>,
}

/// 主屏物理像素尺寸(只用于待机面板的默认摆放;截图请用 [`virtual_screen`])
pub fn screen_size() -> (i32, i32) {
    unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) }
}

/// 虚拟桌面(所有显示器拼合)的物理像素矩形 `(x, y, w, h)`。
/// 多显示器时这是能一次覆盖全部屏幕的区域;副屏在主屏左侧/上方时 x/y 为负。
pub fn virtual_screen() -> (i32, i32, i32, i32) {
    unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    }
}

/// 主屏的物理矩形。主屏左上角恒为 (0,0),尺寸就是 SM_CXSCREEN/SM_CYSCREEN。
pub fn primary_monitor_rect() -> (i32, i32, i32, i32) {
    let (w, h) = screen_size();
    (0, 0, w, h)
}

/// 点所在的显示器(离得最近的一台)。点「开始标注」时用它决定标注哪块屏。
pub fn monitor_from_point(x: i32, y: i32) -> Option<HMONITOR> {
    let h = unsafe { MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST) };
    (!h.is_invalid()).then_some(h)
}

/// 某台显示器的物理矩形(虚拟桌面坐标)。显示器被拔掉时返回 None。
pub fn monitor_rect(hmon: HMONITOR) -> Option<(i32, i32, i32, i32)> {
    let mut mi = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if !unsafe { GetMonitorInfoW(hmon, &mut mi).as_bool() } {
        return None;
    }
    let r = mi.rcMonitor;
    Some((r.left, r.top, r.right - r.left, r.bottom - r.top))
}

/// 鼠标的物理屏幕坐标(拿不到时 None)
pub fn cursor_point() -> Option<(i32, i32)> {
    let mut p = POINT::default();
    unsafe { GetCursorPos(&mut p).ok()? };
    Some((p.x, p.y))
}

/// 所有显示器的物理像素矩形(虚拟桌面坐标)。
/// 注意:虚拟桌面是一个矩形,但显示器之间可能留有空隙(例如上方副屏比主屏窄,
/// 虚拟屏右上角就不属于任何显示器)——画在那里的内容用户根本看不到,摆 UI 时要避开。
pub fn monitor_rects() -> Vec<(i32, i32, i32, i32)> {
    unsafe extern "system" fn collect(
        hmon: HMONITOR,
        _hdc: HDC,
        _rc: *mut RECT,
        data: LPARAM,
    ) -> BOOL {
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(hmon, &mut mi).as_bool() {
            let r = mi.rcMonitor;
            let list = &mut *(data.0 as *mut Vec<(i32, i32, i32, i32)>);
            list.push((r.left, r.top, r.right - r.left, r.bottom - r.top));
        }
        BOOL(1)
    }
    let mut list: Vec<(i32, i32, i32, i32)> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(collect),
            LPARAM(&mut list as *mut _ as isize),
        );
    }
    if list.is_empty() {
        // 枚举失败(理论上不会):退回整块虚拟屏
        let (x, y, w, h) = virtual_screen();
        list.push((x, y, w, h));
    }
    list
}

/// 等 DWM 把已提交的窗口更新真正合成上屏。
/// BitBlt 抓的是合成后的桌面:透明覆盖层刚隐藏时,屏幕上可能还残留上一帧的选框/标注,
/// 必须等合成器消费完帧再抓,否则抓到的仍是旧画面。
pub fn flush_composition() {
    unsafe {
        let _ = DwmFlush();
    }
}

/// 指定物理像素区域 RGBA(框选截图、放大镜用)。
/// `x`/`y` 是虚拟桌面坐标(允许为负);超出虚拟桌面的部分自动裁掉。
pub fn capture_region_px(x: i32, y: i32, w: i32, h: i32, lang: Lang) -> Result<Shot, String> {
    let (vx, vy, vw, vh) = virtual_screen();
    let x0 = x.max(vx);
    let y0 = y.max(vy);
    let w = x.saturating_add(w).min(vx + vw) - x0;
    let h = y.saturating_add(h).min(vy + vh) - y0;
    if w <= 0 || h <= 0 {
        return Err(lang.strings().err_invalid_region.into());
    }
    let (bgra, w, h) = blt_bgra(x0, y0, w, h, lang)?;
    let mut shot = Shot { width: w, height: h, rgba: bgra };
    bgra_to_rgba(&mut shot.rgba);
    Ok(shot)
}

fn blt_bgra(src_x: i32, src_y: i32, w: i32, h: i32, lang: Lang) -> Result<(Vec<u8>, i32, i32), String> {
    unsafe {
        let hdc_screen = GetDC(None);
        if hdc_screen.is_invalid() {
            return Err(lang.strings().err_getdc.into());
        }
        let hdc_mem = CreateCompatibleDC(Some(hdc_screen));
        let hbmp = CreateCompatibleBitmap(hdc_screen, w, h);
        let old = SelectObject(hdc_mem, hbmp.into());

        let blt = BitBlt(hdc_mem, 0, 0, w, h, Some(hdc_screen), src_x, src_y, SRCCOPY | CAPTUREBLT);
        let mut ok = blt.is_ok();

        let mut buf = vec![0u8; w as usize * h as usize * 4];
        if ok {
            let mut bmi = BITMAPINFO::default();
            bmi.bmiHeader = BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // 负值 = 自上而下
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                biSizeImage: (w as u32) * (h as u32) * 4,
                ..Default::default()
            };
            let got = GetDIBits(
                hdc_mem,
                hbmp,
                0,
                h as u32,
                Some(buf.as_mut_ptr().cast()),
                &mut bmi,
                DIB_RGB_COLORS,
            );
            if got == 0 {
                ok = false;
            }
        }

        SelectObject(hdc_mem, old);
        let _ = DeleteObject(hbmp.into());
        let _ = DeleteDC(hdc_mem);
        ReleaseDC(None, hdc_screen);

        if !ok {
            return Err(lang.strings().err_bitblt.into());
        }
        Ok((buf, w, h))
    }
}

/// GDI 出来的是 BGRA 且 alpha 未定义,转成 RGBA 并补成不透明
fn bgra_to_rgba(buf: &mut [u8]) {
    for px in buf.chunks_exact_mut(4) {
        px.swap(0, 2);
        px[3] = 255;
    }
}
