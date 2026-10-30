//! 应用主体:待机(小工具条)⇄ 标注(全屏)两态切换;
//! 待机时窗口就是一个无边框小面板,激活时把同一个窗口拉成全屏,
//! 并铺上进入瞬间冻结的显示器画面(不依赖窗口透明合成)。

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;
use egui::{
    Align2, Color32, ColorImage, Context, CursorIcon, FontData, FontDefinitions, FontFamily,
    FontId, Frame, Id, Key, LayerId, Order, PointerButton, Pos2, Rect, Stroke, TextureOptions,
    Vec2, ViewportCommand,
};
use image::ImageEncoder;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_TRANSITIONS_FORCEDISABLED};
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowRect, SetWindowPos, ShowWindow, SWP_NOACTIVATE, SWP_NOOWNERZORDER, SWP_NOZORDER,
    SW_HIDE, SW_SHOWNOACTIVATE,
};

use crate::bgremove::{self, BgEngine, BgIssue, BgMethod};
use crate::capture;
use crate::i18n::{self, Lang};
use crate::icons::{self, Icon};
use crate::ink::{freehand, with_alpha, Canvas, Ink, Spec, Tool};
use crate::isnet;
use crate::printing;

pub const STANDBY_SIZE: Vec2 = Vec2::new(132.0, 96.0);
const BAR_WIDTH: f32 = 96.0;

/// 左列画笔色 / 右列荧光笔色(对齐原版 5×2 布局)
const PEN_COLORS: [Color32; 5] = [
    Color32::from_rgb(229, 57, 53),   // 红
    Color32::from_rgb(253, 216, 53),  // 黄
    Color32::from_rgb(67, 160, 71),   // 绿
    Color32::from_rgb(30, 136, 229),  // 蓝
    Color32::from_rgb(255, 255, 255), // 白
];
const HL_COLORS: [Color32; 5] = [
    Color32::from_rgb(255, 82, 82),   // 亮红
    Color32::from_rgb(255, 238, 88),  // 亮黄
    Color32::from_rgb(105, 240, 174), // 亮绿
    Color32::from_rgb(64, 196, 255),  // 亮蓝
    Color32::from_rgb(33, 33, 33),    // 黑
];

const TOOLBAR_BG: (u8, u8, u8, u8) = (238, 238, 240, 250);
const STROKE_GRAY: Color32 = Color32::from_rgb(176, 176, 176);
const DARK_TEXT: Color32 = Color32::from_rgb(55, 71, 79);
const RED: Color32 = Color32::from_rgb(211, 47, 47);
const SELECT_FILL: Color32 = Color32::from_rgb(144, 202, 249);
const HOVER_FILL: Color32 = Color32::from_rgb(224, 231, 240);

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Mode {
    Standby,
    Active,
}

/// 截图任务
#[derive(Clone, Copy, PartialEq, Debug)]
enum ShotJob {
    Save,
    Copy,
    Print,
}

/// 截图状态机:Idle → NeedFrame(下一帧渲染时决定是否隐藏工具条) → Sent(已请求帧缓冲截图)
#[derive(Clone, Copy, PartialEq)]
enum ShotPhase {
    Idle,
    NeedFrame,
    Sent,
}

/// 存图格式(WebP 体积明显小于 PNG;PNG 兼容性最好)
#[derive(Clone, Copy, PartialEq, Eq)]
enum ShotFormat {
    Webp,
    Png,
}

impl ShotFormat {
    /// 同一份取值既当文件扩展名,也写进 ui.txt
    fn code(self) -> &'static str {
        match self {
            ShotFormat::Webp => "webp",
            ShotFormat::Png => "png",
        }
    }

    fn from_code(code: &str) -> Option<ShotFormat> {
        match code {
            "webp" => Some(ShotFormat::Webp),
            "png" => Some(ShotFormat::Png),
            _ => None,
        }
    }
}

/// 存图编码设置(设置面板可调,持久化到 ui.txt)
#[derive(Clone, Copy, PartialEq)]
struct ShotSettings {
    format: ShotFormat,
    /// 清晰度 1-100:WebP 有损编码的质量,越高越清晰、文件越大
    quality: u8,
    /// WebP 无损压缩:画质与原图完全一致,体积仍小于 PNG
    lossless: bool,
}

impl Default for ShotSettings {
    fn default() -> Self {
        Self {
            format: ShotFormat::Webp,
            quality: 85,
            lossless: false,
        }
    }
}

pub struct App {
    mode: Mode,
    canvas: Canvas,
    tool: Tool,
    color: Color32,
    /// 左列选中=不透明,右列选中=透明(马克笔)
    opaque: bool,
    width: f32,
    drawing: bool,
    text_edit: Option<TextEditor>,
    toast: Option<(String, Instant)>,
    placed: bool,
    // 框选截图
    region_armed: bool,
    region_sel: Option<RegionSel>,
    /// 选框区域缩放倍率(1=原始,2/4/8/10)
    region_zoom: f32,
    zoom_base: Option<egui::TextureHandle>,
    zoom_dirty: bool,
    /// 选区去背方式(键控 / ISNet / 自动)
    bg_method: BgMethod,
    /// 键控去背时把深底上的浅色文字反相压暗(白底可读性)
    dark_light: bool,
    /// 后台去背任务 id(等待结果;结果回来后才替换选区显示)
    bg_pending: Option<u64>,
    /// 发起任务时的选区:等待期间选区变了就丢弃这份结果
    bg_pending_sel: Option<RegionSel>,
    /// 任务开始时间(处理中提示显示已等待秒数)
    bg_pending_since: Option<Instant>,
    /// 选区去背后的白底画面(替换选区显示;存图/复制/打印直接生效)
    region_clean: Option<egui::TextureHandle>,
    /// 进入标注时冻结的桌面画面(不透明底图)。
    /// 原版 Pointofix 进入标注时也是冻结当前画面;更重要的是,
    /// wgpu 的透明窗口在部分 AMD/NVIDIA 驱动和远程桌面下会整体渲染成黑色,
    /// 铺一张底图就不依赖窗口透明合成了。
    backdrop: Option<egui::TextureHandle>,
    /// 已点击「开始标注」:窗口已藏起来,等下一帧 DWM 合成完再抓底图
    pending_entry: bool,
    // 放大镜
    magnifier: bool,
    zoom: f32,
    mag_tex: Option<egui::TextureHandle>,
    // 设置(语言 + 关于)
    show_settings: bool,
    lang: Lang,
    /// 签名章文字(设置面板可改,跨启动保留)
    signature: String,
    /// 存图格式 / 清晰度(设置面板)
    shot: ShotSettings,
    // 橡皮
    eraser_last: Option<Pos2>,
    // 可拖动位置(逻辑坐标,跨启动保留)
    bar_pos: Option<Pos2>,
    /// 待机面板位置(物理像素,跨启动保留;换显示器/DPI 都不漂移)
    standby_px: Option<Pos2>,
    /// 覆盖层窗口句柄:按物理像素精确铺到目标显示器上
    hwnd: Option<HWND>,
    /// 标注目标显示器(「开始标注」按钮所在的那块屏)的物理矩形;待机时为主屏。
    /// 覆盖层只铺这一块屏,窗口内坐标 = 这块屏内的坐标。
    screen: (i32, i32, i32, i32),
    /// 本次标注锁定的显示器句柄(显示器改分辨率时按它重新取矩形)
    active_mon: Option<HMONITOR>,
    /// 虚拟桌面(所有显示器拼合)物理矩形,用于发现显示器布局变化
    virtual_rect: (i32, i32, i32, i32),
    /// 各显示器物理矩形(虚拟桌面坐标),用于把待机面板吸附到真实屏幕上
    monitors: Vec<(i32, i32, i32, i32)>,
    /// 上次铺屏时的窗口 DPI:DPI 变化后需要重新铺满(winit 会按逻辑尺寸缩放窗口)
    layout_ppp: f32,
    /// 工具条在缩放 1.0 时的自然高度(logical),由上一帧实测得到
    bar_unit_h: f32,
    /// 工具条缩放:显示器太矮装不下整条工具条时按比例缩小
    bar_scale: f32,
    /// POINTTOFIX_GEOM_LOG 时的日志节流计数
    geom_log_frames: u32,
    /// 自检:进入标注模式 N 帧后自动触发去背(模拟点击 ⊠)
    selftest_countdown: u32,
    // 截图合成管线(DWM 透明窗口不在 BitBlt 里,需合入 egui 帧缓冲)
    shot_job: Option<ShotJob>,
    shot_phase: ShotPhase,
    shot_slot: Arc<Mutex<Option<(ShotJob, Arc<ColorImage>)>>>,
    shot_skip_chrome: bool,
    /// 去背抓图帧:隐藏标注层(标注是独立矢量层,不进背景处理)
    shot_skip_ink: bool,
    /// 待机面板 / 工具条位置
    shot_sent_wait: u32,
    /// 去背进行中:隐藏覆盖层,等干净画面真正合成上屏后再抓屏(抓屏会连带覆盖层一起抓)
    debak_started: Option<Instant>,
    /// 去背至少还要再画几帧干净画面,保证有完整帧被呈现
    debak_frames: u32,
    /// 调试模式:截图包含工具条/选框等界面元素
    debug_shot: bool,
    /// 自检:进入标注模式 N 帧后自动存一张图(验证截图合成管线)
    autosave_frames: u32,
    /// 自检:进入标注模式 N 帧后自动 Ctrl+C(验证剪贴板链路)
    autocopy_frames: u32,
    /// 自检:POINTTOFIX_STAMPTEST=1 时,激活后 N 帧自动盖勾/叉章(验证盖章链路)
    stamp_frames: u32,
}

pub struct TextEditor {
    pos: Pos2,
    buf: String,
    focused: bool,
}

#[derive(Clone, Copy, PartialEq)]
struct RegionSel {
    start: Pos2,
    cur: Pos2,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::light());
        load_cjk_font(&cc.egui_ctx);
        let (standby_px, bar_pos, debug_shot, lang, shot, bg_method, dark_light, signature) =
            load_ui_state();
        // 铺满虚拟屏要按物理像素精确定位,egui 的视口命令只有逻辑坐标(要经过窗口 DPI 换算),
        // 跨不同 DPI 的显示器会偏,所以这里留一份 HWND 直接 SetWindowPos
        let hwnd = cc.window_handle().ok().and_then(|handle| match handle.as_raw() {
            RawWindowHandle::Win32(h) => Some(HWND(h.hwnd.get() as *mut _)),
            _ => None,
        });
        Self {
            mode: Mode::Standby,
            canvas: Canvas::default(),
            tool: Tool::Pen,
            color: PEN_COLORS[0],
            opaque: true,
            width: 4.0,
            drawing: false,
            text_edit: None,
            toast: None,
            placed: false,
            region_armed: false,
            region_sel: None,
            region_zoom: 1.0,
            zoom_base: None,
            zoom_dirty: false,
            bg_method,
            dark_light,
            bg_pending: None,
            bg_pending_sel: None,
            bg_pending_since: None,
            region_clean: None,
            backdrop: None,
            pending_entry: false,
            magnifier: false,
            zoom: 2.5,
            mag_tex: None,
            show_settings: false,
            lang,
            signature,
            shot,
            eraser_last: None,
            bar_pos,
            standby_px,
            hwnd,
            screen: capture::primary_monitor_rect(),
            active_mon: None,
            virtual_rect: capture::virtual_screen(),
            monitors: capture::monitor_rects(),
            layout_ppp: 1.0,
            bar_unit_h: 620.0,
            bar_scale: 1.0,
            geom_log_frames: 0,
            selftest_countdown: 0,
            shot_job: None,
            shot_phase: ShotPhase::Idle,
            shot_slot: Arc::new(Mutex::new(None)),
            shot_skip_chrome: false,
            shot_skip_ink: false,
            shot_sent_wait: 0,
            debak_started: None,
            debak_frames: 0,
            debug_shot,
            autosave_frames: 0,
            autocopy_frames: 0,
            stamp_frames: 0,
        }
    }

    // ---------- 窗口几何(多显示器) ----------

    /// 当前标注显示器的逻辑尺寸(egui 坐标)= 物理像素 ÷ 窗口 DPI。
    /// 覆盖层只铺这一块屏,所以它同时也是窗口内可用的全部范围。
    fn screen_logical(&self, ctx: &Context) -> Vec2 {
        let (_, _, w, h) = self.screen;
        let ppp = ctx.pixels_per_point().max(0.1);
        Vec2::new(w as f32 / ppp, h as f32 / ppp)
    }

    /// 把标注层铺到目标显示器上(只铺这一块屏,不跨屏)。
    /// 直接对 HWND 按物理像素 SetWindowPos:窗口无边框,客户区即窗口区,
    /// 不受"逻辑坐标 × 窗口 DPI"换算影响。目标矩形由 `self.screen` 给出。
    fn fit_overlay_to_monitor(&mut self, ctx: &Context) {
        let (vx, vy, vw, vh) = self.screen;
        self.layout_ppp = ctx.pixels_per_point();
        if let Some(hwnd) = self.hwnd {
            let _ = unsafe {
                SetWindowPos(
                    hwnd,
                    None,
                    vx,
                    vy,
                    vw,
                    vh,
                    SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
                )
            };
        } else {
            // 极端情况下拿不到 HWND(窗口尚未创建):退回视口命令,精度差一点但能用
            let ppp = self.layout_ppp.max(0.1);
            let logical = |v: i32| v as f32 / ppp;
            ctx.send_viewport_cmd(ViewportCommand::OuterPosition(Pos2::new(
                logical(vx),
                logical(vy),
            )));
            ctx.send_viewport_cmd(ViewportCommand::InnerSize(Vec2::new(logical(vw), logical(vh))));
        }
        // 多显示器排查用:POINTTOFIX_GEOM_LOG=1 打印每次铺屏后的真实窗口矩形
        if std::env::var("POINTTOFIX_GEOM_LOG").is_ok() {
            let actual = self.hwnd.and_then(|h| {
                let mut r = windows::Win32::Foundation::RECT::default();
                unsafe { GetWindowRect(h, &mut r).ok()? };
                Some((r.left, r.top, r.right - r.left, r.bottom - r.top))
            });
            eprintln!(
                "[geom] target=({vx},{vy},{vw},{vh}) ppp={} window={actual:?}",
                ctx.pixels_per_point()
            );
        }
    }

    /// 抓当前目标显示器的画面,作为标注的不透明底图。
    /// 抓之前先隐藏窗口(并且等 DWM 合成完),否则待机面板会一起被拍进去、
    /// 重铺显示器时还会把工具栏/标注拍进去。
    fn grab_backdrop(&self) -> Option<capture::Shot> {
        let hwnd = self.hwnd?; // 拿不到 HWND 就没法先隐藏窗口,放弃底图(退回透明覆盖)
        let (x, y, w, h) = self.screen;
        if w <= 0 || h <= 0 {
            return None;
        }
        disable_transitions(hwnd);
        unsafe {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
        capture::flush_composition();
        capture::capture_region_px(x, y, w, h, self.lang).ok()
    }

    /// grab_backdrop 把窗口藏起来了,铺好屏幕后要重新显示
    fn show_overlay(&self) {
        if let Some(hwnd) = self.hwnd {
            unsafe {
                let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            }
        }
    }

    /// 按物理像素移动窗口:egui 的位置命令会乘当前窗口 DPI,先除回去即可精确落位,
    /// 于是待机面板从小 DPI 屏挪到大 DPI 屏时位置也不会漂
    fn move_window_px(&self, ctx: &Context, p: Pos2) {
        let ppp = ctx.pixels_per_point().max(0.1);
        ctx.send_viewport_cmd(ViewportCommand::OuterPosition(Pos2::new(p.x / ppp, p.y / ppp)));
    }

    /// 把物理像素矩形 [p, p+size] 吸附到某台显示器内。
    /// 只夹到虚拟桌面矩形是不够的:多屏之间有空隙(上方副屏比主屏窄时,
    /// 虚拟屏右上角就不属于任何显示器),UI 摆进去会整块看不见。
    /// 优先取包含中心的显示器,否则取最近的一台,再夹到它的范围内。
    fn snap_px_to_monitor(&self, p: Pos2, size: Vec2) -> Pos2 {
        let monitors = &self.monitors;
        let center = Pos2::new(p.x + size.x / 2.0, p.y + size.y / 2.0);
        let inside = |m: &(i32, i32, i32, i32)| {
            center.x >= m.0 as f32
                && center.x <= (m.0 + m.2) as f32
                && center.y >= m.1 as f32
                && center.y <= (m.1 + m.3) as f32
        };
        let idx = monitors.iter().position(inside).unwrap_or_else(|| {
            monitors
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| {
                    let d = |m: &(i32, i32, i32, i32)| {
                        let cx = m.0 as f32 + m.2 as f32 / 2.0 - center.x;
                        let cy = m.1 as f32 + m.3 as f32 / 2.0 - center.y;
                        cx * cx + cy * cy
                    };
                    d(a).total_cmp(&d(b))
                })
                .map(|(i, _)| i)
                .unwrap_or(0)
        });
        let (mx, my, mw, mh) = monitors[idx];
        let max_x = ((mx + mw) as f32 - size.x).max(mx as f32);
        let max_y = ((my + mh) as f32 - size.y).max(my as f32);
        Pos2::new(p.x.clamp(mx as f32, max_x), p.y.clamp(my as f32, max_y))
    }

    /// 待机面板的物理坐标限制在某台显示器内(拔显示器/改分辨率后不会跑到屏幕外或空隙里)
    fn clamp_standby_px(&self, p: Pos2, ppp: f32) -> Pos2 {
        let margin = 8.0 * ppp;
        let size = Vec2::new(
            STANDBY_SIZE.x * ppp + margin,
            STANDBY_SIZE.y * ppp + margin,
        );
        self.snap_px_to_monitor(p, size)
    }

    /// 跟随显示器变化(热插拔 / 改分辨率 / 改缩放):
    /// 待机时刷新显示器列表(面板吸附用);标注时按锁定的显示器重新取矩形并铺满
    /// (DPI 变化时 winit 会按逻辑尺寸缩放窗口,必须再按物理像素铺一次)
    fn sync_screen_layout(&mut self, ctx: &Context) {
        let vrect = capture::virtual_screen();
        let layout_changed = vrect != self.virtual_rect;
        if layout_changed {
            self.virtual_rect = vrect;
            self.monitors = capture::monitor_rects();
        }
        if self.mode != Mode::Active {
            return;
        }
        let rect = self.active_mon.and_then(capture::monitor_rect);
        if rect.is_none() {
            self.active_mon = None; // 目标显示器被拔掉了:退回主屏
        }
        let rect = rect.unwrap_or_else(capture::primary_monitor_rect);
        let ppp = ctx.pixels_per_point();
        if layout_changed || rect != self.screen || (ppp - self.layout_ppp).abs() > 0.001 {
            self.screen = rect;
            // 显示器/分辨率/DPI 变了:旧底图作废,重新冻结当前画面
            self.backdrop = self.grab_backdrop().map(|shot| {
                let image = ColorImage::from_rgba_unmultiplied(
                    [shot.width as usize, shot.height as usize],
                    &shot.rgba,
                );
                ctx.load_texture("backdrop", image, TextureOptions::LINEAR)
            });
            self.fit_overlay_to_monitor(ctx);
            self.show_overlay();
        }
    }

    // ---------- 状态切换 ----------

    /// 点击「开始标注」:锁定目标显示器,藏起待机面板,推迟到下一帧再抓底图和铺屏。
    /// 中间隔一帧是必要的:ShowWindow(SW_HIDE) 后 DWM 要下一次合成才把面板
    /// 从屏幕上抹掉,立刻 BitBlt 抓到的画面里还会留着面板残影。
    fn begin_enter_active(&mut self, ctx: &Context) {
        self.region_zoom = 1.0;
        // 只标注「开始标注」按钮所在的那块显示器。按钮在待机面板上,
        // 用面板中心的物理坐标找显示器;拿不到就退鼠标位置,再退主屏。
        let ppp = ctx.pixels_per_point();
        let anchor = self
            .standby_px
            .map(|p| {
                (
                    (p.x + STANDBY_SIZE.x * ppp / 2.0) as i32,
                    (p.y + STANDBY_SIZE.y * ppp / 2.0) as i32,
                )
            })
            .or_else(capture::cursor_point)
            .unwrap_or((0, 0));
        self.active_mon = capture::monitor_from_point(anchor.0, anchor.1);
        self.screen = self
            .active_mon
            .and_then(capture::monitor_rect)
            .unwrap_or_else(capture::primary_monitor_rect);
        self.pending_entry = true;
        if let Some(hwnd) = self.hwnd {
            disable_transitions(hwnd);
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
        }
        ctx.request_repaint();
    }

    fn enter_active(&mut self, ctx: &Context) {
        self.pending_entry = false;
        self.mode = Mode::Active;
        self.region_zoom = 1.0;
        // 底图:窗口已经藏了整整一帧,画面里不会带待机面板。
        // 这张底图让标注层不再依赖窗口透明:部分 AMD/NVIDIA 驱动和远程桌面
        // 会把透明 wgpu 窗口合成成纯黑,铺上底图后最坏也只是"冻结的画面"。
        self.backdrop = self.grab_backdrop().map(|shot| {
            let image = ColorImage::from_rgba_unmultiplied(
                [shot.width as usize, shot.height as usize],
                &shot.rgba,
            );
            ctx.load_texture("backdrop", image, TextureOptions::LINEAR)
        });
        self.fit_overlay_to_monitor(ctx);
        self.show_overlay();
        // 恢复上次的选框区域(默认显示选框,不进入拖拽定义模式);超出当前屏幕的部分裁掉
        let ls = self.screen_logical(ctx);
        let (lw, lh) = (ls.x, ls.y);
        if let Some((x, y, rw, rh)) = load_saved_region(ctx.pixels_per_point()) {
            let x = x.clamp(0.0, (lw - 8.0).max(0.0));
            let y = y.clamp(0.0, (lh - 8.0).max(0.0));
            let rw = rw.min(lw - x);
            let rh = rh.min(lh - y);
            if rw > 4.0 && rh > 4.0 {
                self.region_sel = Some(RegionSel {
                    start: Pos2::new(x, y),
                    cur: Pos2::new(x + rw, y + rh),
                });
            }
        }
        ctx.send_viewport_cmd(ViewportCommand::Focus);
        // 调试:POINTTOFIX_DEBACK=1 启动即对恢复的选区执行去背(自动化测试用)
        if std::env::var("POINTTOFIX_DEBACK").is_ok() {
            self.toggle_region_clean(ctx);
        }
        // 自检:POINTTOFIX_CLICKTEST=1 时,激活后第 10 帧调用 ⊠ 按钮同一处理函数
        if std::env::var("POINTTOFIX_CLICKTEST").is_ok() {
            self.selftest_countdown = 10;
        }
        // 自检:POINTTOFIX_AUTOSAVE=1 时,激活后约 2 秒自动执行一次「存图」
        if std::env::var("POINTTOFIX_AUTOSAVE").is_ok() {
            self.autosave_frames = 80;
        }
        // 自检:POINTTOFIX_AUTOCOPY=1 时,激活后约 2 秒自动执行一次 Ctrl+C
        if std::env::var("POINTTOFIX_AUTOCOPY").is_ok() {
            self.autocopy_frames = 100;
        }
        // 自检:POINTTOFIX_STAMPTEST=1 时,激活后自动盖勾/叉章并自动存图(验证盖章链路)
        if std::env::var("POINTTOFIX_STAMPTEST").is_ok() {
            self.stamp_frames = 40;
            self.autosave_frames = 100;
        }
    }

    fn exit_active(&mut self, ctx: &Context) {
        // 保存选区供下次启动恢复
        save_region_state(self.region_sel, ctx.pixels_per_point());
        self.canvas.clear();
        self.text_edit = None;
        self.drawing = false;
        self.region_armed = false;
        self.region_sel = None;
        self.region_zoom = 1.0;
        self.zoom_base = None;
        self.zoom_dirty = false;
        self.region_clean = None;
        self.bg_pending = None;
        self.bg_pending_sel = None;
        self.bg_pending_since = None;
        self.backdrop = None;
        self.debak_started = None;
        self.debak_frames = 0;
        self.magnifier = false;
        self.show_settings = false;
        self.mode = Mode::Standby;
        self.active_mon = None;
        self.screen = capture::primary_monitor_rect();
        // 回到进入标注前的位置(多显示器时不会莫名其妙跳回主屏);没有记录才放主屏右上角
        let ppp = ctx.pixels_per_point();
        let pos_px = self.standby_px.unwrap_or_else(|| {
            let (w, _) = capture::screen_size();
            Pos2::new((w as f32 - (STANDBY_SIZE.x + 8.0) * ppp).max(8.0 * ppp), 8.0 * ppp)
        });
        self.move_window_px(ctx, self.clamp_standby_px(pos_px, ppp));
        ctx.send_viewport_cmd(ViewportCommand::InnerSize(STANDBY_SIZE));
    }

    // ---------- 待机界面 ----------

    fn standby_ui(&mut self, ctx: &Context, ui: &mut egui::Ui) {
        let tr = self.lang.strings();
        egui::CentralPanel::default()
            .frame(
                Frame::NONE
                    .fill(Color32::from_rgba_unmultiplied(240, 240, 242, 248))
                    .corner_radius(egui::epaint::CornerRadius::same(8)),
            )
            .show(ui, |ui| {
                // 顶部拖动条:按住可移动小面板
                let grip = ui.allocate_response(
                    egui::vec2(ui.available_width(), 12.0),
                    egui::Sense::drag(),
                );
                if grip.hovered() {
                    ctx.set_cursor_icon(egui::CursorIcon::Grab);
                }
                if grip.drag_started() {
                    ctx.send_viewport_cmd(ViewportCommand::StartDrag);
                }
                // 顶行:标题 + 右上角关闭(×)
                ui.horizontal(|ui| {
                    ui.add_space(6.0);
                    ui.label(egui::RichText::new("PointToFix").strong().size(12.0));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(2.0);
                        let close = ui.add_sized(
                            [20.0, 20.0],
                            egui::Button::new(egui::RichText::new("×").size(15.0)),
                        );
                        if close.on_hover_text(tr.exit_program_tip).clicked() {
                            ctx.send_viewport_cmd(ViewportCommand::Close);
                        }
                    });
                });
                ui.add_space(6.0);
                ui.vertical_centered(|ui| {
                    if ui
                        .add_sized([104.0, 34.0], egui::Button::new(tr.start_annotate))
                        .clicked()
                    {
                        self.begin_enter_active(ctx);
                    }
                });
            });
        // 记录待机面板位置(拖动后跨启动保留)。存物理像素:面板挪到不同 DPI 的屏幕后
        // 逻辑坐标会随窗口 DPI 缩放,存逻辑值下次启动就会漂。
        // pending_entry 帧面板已经藏起来、正要去标注,这一帧的窗口位置没有意义
        // (POINTTOFIX_START_ACTIVE 时还是创建时的默认位置),不要覆盖已保存的位置。
        if !self.pending_entry {
            if let Some(outer) = ctx.input(|i| i.viewport().outer_rect) {
                let ppp = ctx.pixels_per_point();
                let p = Pos2::new(outer.min.x * ppp, outer.min.y * ppp);
                if self.standby_px.is_none_or(|q| q.distance(p) > 0.5) {
                    self.standby_px = Some(p);
                    self.save_state();
                }
            }
        }
    }

    // ---------- 标注界面 ----------

    fn active_ui(&mut self, ctx: &Context) {
        let tr = self.lang.strings();
        // 画布画在背景层,工具条等 egui 浮窗在其上
        let canvas_painter = ctx.layer_painter(LayerId::new(Order::Background, Id::unique("canvas")));
        // 冻结的桌面底图:最底层,不随 shot_skip_ink/chrome 隐藏
        // (去背抓图、存图时它必须留在画面上,否则会抓到黑色/透明)
        if let Some(tex) = &self.backdrop {
            canvas_painter.image(
                tex.id(),
                Rect::from_min_size(Pos2::ZERO, self.screen_logical(ctx)),
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        if !self.shot_skip_ink {
            // 去背后的白底画面垫在墨迹下面:去背完成后显示,之后画的标注浮在白底之上。
            // 必须与墨迹画在同一层(且先画),否则 Middle 层的白底会盖住 Background 层的标注。
            if let (Some(sel), Some(tex)) = (self.region_sel, self.region_clean.as_ref()) {
                let r = Rect::from_two_pos(sel.start, sel.cur);
                canvas_painter.image(
                    tex.id(),
                    r,
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
            // 后台去背进行中:在选区中央显示进度(截图帧隐藏,不会进导出图)
            if !self.shot_skip_chrome {
                if let (Some(sel), true) = (self.region_sel, self.bg_pending.is_some()) {
                    let r = Rect::from_two_pos(sel.start, sel.cur);
                    let secs = self
                        .bg_pending_since
                        .map(|t| t.elapsed().as_secs())
                        .unwrap_or(0);
                    let label = if secs > 0 {
                        format!("{} ({}s)", tr.debak_processing, secs)
                    } else {
                        tr.debak_processing.to_string()
                    };
                    let painter =
                        ctx.layer_painter(LayerId::new(Order::Middle, Id::unique("bg_processing")));
                    let galley = painter.layout_no_wrap(
                        label,
                        FontId::proportional(15.0),
                        Color32::WHITE,
                    );
                    let box_rect = Rect::from_center_size(
                        r.center(),
                        galley.size() + egui::vec2(24.0, 14.0),
                    );
                    painter.rect_filled(box_rect, 6.0, Color32::from_black_alpha(150));
                    painter.galley(
                        box_rect.center() - galley.size() * 0.5,
                        galley,
                        Color32::WHITE,
                    );
                }
            }
            // 有选区时墨迹裁剪到选框内:超出选框的标注部分不显示(与导出画面一致)
            if let Some(sel) = self.region_sel {
                let r = Rect::from_two_pos(sel.start, sel.cur);
                self.canvas.paint(&canvas_painter.with_clip_rect(r));
            } else {
                self.canvas.paint(&canvas_painter);
            }
        }
        // 光标:文字编辑时用输入光标;悬停在画布上且当前是文字工具时也用输入光标
        // (预览"点击这里开始输入");其余保持十字光标。
        // 注意在 shot_skip_chrome 帧(截图/去背)不能改光标,避免光标进导出图
        let text_tool_hover = !self.shot_skip_chrome
            && self.text_edit.is_none()
            && self.tool == Tool::Text
            && !ctx.is_pointer_over_egui();
        if self.text_edit.is_some() || text_tool_hover {
            ctx.set_cursor_icon(CursorIcon::Text);
        } else {
            ctx.set_cursor_icon(CursorIcon::Crosshair);
        }

        // 框选矩形可视化(白线 + 外圈黑线;已有选区时标注尺寸)——截图帧隐藏
        if !self.shot_skip_chrome {
            if let Some(sel) = &self.region_sel {
                let r = Rect::from_two_pos(sel.start, sel.cur);
                canvas_painter.rect_stroke(r, 0.0, Stroke::new(2.0, Color32::BLACK), egui::epaint::StrokeKind::Outside);
                canvas_painter.rect_stroke(r, 0.0, Stroke::new(1.0, Color32::WHITE), egui::epaint::StrokeKind::Inside);
                let ppp = ctx.pixels_per_point();
                let label = format!("{} × {} px", (r.width() * ppp) as i32, (r.height() * ppp) as i32);
                let pos = r.left_top() + egui::vec2(1.0, -6.0);
                canvas_painter.text(pos + egui::vec2(1.0, 1.0), Align2::LEFT_BOTTOM, &label, FontId::proportional(13.0), Color32::BLACK);
                canvas_painter.text(pos, Align2::LEFT_BOTTOM, &label, FontId::proportional(13.0), Color32::WHITE);
            }
        }

        if !self.shot_skip_chrome {
            self.toolbar_ui(ctx);
            self.text_edit_ui(ctx);
            self.settings_ui(ctx);
        }

        // 橡皮擦圆形光标:圈出实际擦除范围(导出/截图帧隐藏,普通屏幕截图可见)
        if !self.shot_skip_chrome && self.tool == Tool::Eraser && !ctx.is_pointer_over_egui() {
            if let Some(pos) = ctx.input(|i| i.pointer.hover_pos()) {
                let r = self.eraser_radius();
                canvas_painter.circle_stroke(
                    pos,
                    r + 0.5,
                    Stroke::new(3.0, Color32::from_black_alpha(90)),
                );
                canvas_painter.circle_stroke(pos, r, Stroke::new(1.5, Color32::WHITE));
            }
        }

        let esc = ctx.input(|i| i.key_pressed(Key::Escape));
        let over_ui = ctx.is_pointer_over_egui();
        let editing = self.text_edit.is_some();
        let down = ctx.input(|i| i.pointer.button_down(PointerButton::Primary));
        let pos = ctx.input(|i| i.pointer.interact_pos());
        let shift = ctx.input(|i| i.modifiers.shift);

        if esc {
            // 优先级:正在框选拖拽 > 已有选区 > 文字编辑 > 放大镜 > 退出标注
            if self.region_armed {
                self.region_armed = false;
                self.region_sel = None;
            } else if self.region_sel.take().is_some() {
                // 清除选区,恢复全屏保存/复制
                self.region_zoom = 1.0;
                self.zoom_base = None;
                self.region_clean = None;
                self.bg_pending = None;
                self.bg_pending_sel = None;
                self.bg_pending_since = None;
                self.debak_started = None;
                self.debak_frames = 0;
            } else if self.text_edit.take().is_some() {
                // 取消文字编辑
            } else if self.magnifier {
                self.magnifier = false;
            } else {
                self.canvas.current = None;
                self.drawing = false;
                self.exit_active(ctx);
            }
            return;
        }

        // 工具热键(文字编辑时不触发):Ctrl+R 框选 / Ctrl+M 放大镜 / Ctrl+I 设置
        // Ctrl+S 保存 / Ctrl+C 复制(有选区时只作用于选区)
        // 必须用 Ctrl 组合键:中文 IME 开启时裸字母键会被输入法组合输入吃掉
        if !editing {
            let mut save_now = false;
            let mut copy_now = false;
            let mut undo_now = false;
            let mut redo_now = false;
            let mut clean_now = false;
            ctx.input(|i| {
                if std::env::var("POINTTOFIX_KEY_LOG").is_ok() {
                    let keys: Vec<_> = i
                        .events
                        .iter()
                        .filter_map(|e| match e {
                            egui::Event::Key {
                                key,
                                pressed: true,
                                modifiers,
                                ..
                            } => Some((*key, modifiers.ctrl, modifiers.command)),
                            egui::Event::Copy => Some((Key::C, true, true)),
                            _ => None,
                        })
                        .collect();
                    if !keys.is_empty() {
                        eprintln!("[key] {keys:?}");
                    }
                }
                // Ctrl+C / Ctrl+X 会被 egui-winit 转成 Copy/Cut 事件,不再有 Key::C;
                // 所以复制快捷键必须同时看 Event::Copy(文本编辑时整个热键块会被跳过)
                let copy_event = i
                    .events
                    .iter()
                    .any(|e| matches!(e, egui::Event::Copy));
                if copy_event {
                    copy_now = true;
                }
                if !i.modifiers.ctrl {
                    return;
                }
                if i.key_pressed(Key::R) {
                    self.region_armed = !self.region_armed;
                    self.region_sel = None;
                }
                if i.key_pressed(Key::M) {
                    self.magnifier = !self.magnifier;
                }
                if i.key_pressed(Key::I) {
                    self.show_settings = !self.show_settings;
                }
                if i.key_pressed(Key::S) {
                    save_now = true;
                }
                if i.key_pressed(Key::C) {
                    copy_now = true;
                }
                if i.key_pressed(Key::Z) {
                    undo_now = true;
                }
                if i.key_pressed(Key::Y) {
                    redo_now = true;
                }
                if i.key_pressed(Key::D) {
                    clean_now = true;
                }
            });
            if save_now {
                self.do_save(ctx);
            }
            if copy_now {
                self.do_copy(ctx);
            }
            if undo_now {
                self.canvas.undo();
            }
            if redo_now {
                self.canvas.redo();
            }
            if clean_now {
                self.toggle_region_clean(ctx);
            }
        }

        if self.region_armed {
            self.region_input(ctx, over_ui, down, pos);
        } else {
            if std::env::var("POINTTOFIX_GEOM_LOG").is_ok() && down {
                eprintln!(
                    "[input] down={down} over_ui={over_ui} editing={editing} pos={pos:?} drawing={} tool={:?}",
                    self.drawing, self.tool
                );
            }
            if !down || over_ui || editing {
                if self.drawing {
                    self.drawing = false;
                    self.eraser_last = None;
                    self.canvas.commit();
                }
            } else if let Some(pos) = pos {
                if self.tool == Tool::Eraser {
                    // 橡皮擦:沿拖动路径擦除(自由笔切断,其它图形命中即删除)
                    if !self.drawing {
                        self.drawing = true;
                        self.eraser_last = Some(pos);
                        self.canvas.erase(pos, pos, self.eraser_radius());
                    } else {
                        let from = self.eraser_last.unwrap_or(pos);
                        self.canvas.erase(from, pos, self.eraser_radius());
                        self.eraser_last = Some(pos);
                    }
                } else if !self.drawing {
                    self.drawing = true;
                    self.begin_stroke(pos);
                    if self.text_edit.is_some() {
                        self.drawing = false; // 文字工具:打开编辑框,不进入拖拽
                    }
                } else if self.canvas.current.is_some() {
                    self.drag_stroke(pos, shift);
                }
            }
        }

        if self.magnifier && !self.shot_skip_chrome {
            self.magnifier_ui(ctx, over_ui, pos);
        }
        if !self.shot_skip_chrome {
            self.region_zoom_ui(ctx);
        }

        // 自检倒计时:到达 0 时调用 ⊠ 去背的同一入口(验证按钮处理路径,不依赖鼠标)
        if self.selftest_countdown > 0 {
            self.selftest_countdown -= 1;
            ctx.request_repaint();
            if self.selftest_countdown == 0 {
                self.toggle_region_clean(ctx);
            }
        }
        // 自检倒计时:自动存图
        if self.autosave_frames > 0 {
            self.autosave_frames -= 1;
            ctx.request_repaint();
            if self.autosave_frames == 0 {
                if std::env::var("POINTTOFIX_GEOM_LOG").is_ok() {
                    eprintln!("[autosave] firing, shapes={}", self.canvas.shapes.len());
                }
                self.start_shot(ShotJob::Save, ctx);
            }
        }
        // 自检倒计时:自动盖勾/叉章(屏幕中央,验证盖章显示与导出)
        if self.stamp_frames > 0 {
            self.stamp_frames -= 1;
            ctx.request_repaint();
            if self.stamp_frames == 0 {
                let ls = self.screen_logical(ctx);
                let c = Pos2::new(ls.x * 0.5, ls.y * 0.5);
                self.tool = Tool::CheckStamp;
                self.width = 4.0;
                self.begin_stroke(c);
                self.tool = Tool::CrossStamp;
                self.begin_stroke(c + egui::vec2(150.0, 0.0));
                self.tool = Tool::Time;
                self.begin_stroke(c + egui::vec2(-80.0, 110.0));
                // 签名章:没设置时用默认文本,保证自检一定能盖出来
                if self.signature.trim().is_empty() {
                    self.signature = "PointToFix".to_string();
                }
                self.tool = Tool::Signature;
                self.begin_stroke(c + egui::vec2(-80.0, -120.0));
                if std::env::var("POINTTOFIX_GEOM_LOG").is_ok() {
                    eprintln!("[stamp] placed, shapes={}", self.canvas.shapes.len());
                }
            }
        }
        // 自检倒计时:自动复制
        if self.autocopy_frames > 0 {
            self.autocopy_frames -= 1;
            ctx.request_repaint();
            if self.autocopy_frames == 0 {
                self.start_shot(ShotJob::Copy, ctx);
            }
        }
    }

    fn begin_stroke(&mut self, pos: Pos2) {
        let spec = Spec { color: self.effective_color(), width: self.width };
        self.canvas.current = match self.tool {
            Tool::Pen => Some(freehand(spec, pos)),
            Tool::Eraser => None, // 橡皮不走图形管线
            Tool::Line => Some(Ink::Line { a: pos, b: pos, spec }),
            Tool::Arrow => Some(Ink::Arrow { a: pos, b: pos, spec, double: false }),
            Tool::DoubleArrow => Some(Ink::Arrow { a: pos, b: pos, spec, double: true }),
            Tool::Rect => Some(Ink::Rect { a: pos, b: pos, spec, filled: false }),
            Tool::RectFilled => Some(Ink::Rect { a: pos, b: pos, spec, filled: true }),
            Tool::Ellipse => Some(Ink::Ellipse { a: pos, b: pos, spec, filled: false }),
            Tool::EllipseFilled => Some(Ink::Ellipse { a: pos, b: pos, spec, filled: true }),
            Tool::Text => {
                self.text_edit = Some(TextEditor { pos, buf: String::new(), focused: false });
                None
            }
            Tool::CheckStamp => {
                // 绿勾盖章:大小跟随笔宽档位
                let size = (self.width * 9.0).clamp(24.0, 140.0);
                self.canvas.push_instant(Ink::Stamp {
                    pos,
                    size,
                    color: Color32::from_rgb(34, 197, 94),
                    check: true,
                });
                None
            }
            Tool::CrossStamp => {
                // 红叉盖章:大小跟随笔宽档位
                let size = (self.width * 9.0).clamp(24.0, 140.0);
                self.canvas.push_instant(Ink::Stamp {
                    pos,
                    size,
                    color: Color32::from_rgb(220, 38, 38),
                    check: false,
                });
                None
            }
            Tool::Time => {
                // 时间章:点击那刻的日期+整点,字号跟随笔宽档位
                let size = (self.width * 4.5).clamp(15.0, 44.0);
                self.canvas.push_instant(Ink::Seal {
                    pos,
                    size,
                    color: Color32::from_rgb(220, 38, 38),
                    text: local_time_stamp_text(self.lang),
                });
                None
            }
            Tool::Signature => {
                // 签名章:文字来自设置;没填就提示,不盖空白章
                let text = self.signature.trim().to_string();
                if text.is_empty() {
                    let tr = self.lang.strings();
                    self.set_toast(Err(tr.signature_empty_tip.to_string()));
                    None
                } else {
                    let size = (self.width * 4.5).clamp(15.0, 44.0);
                    self.canvas.push_instant(Ink::Seal {
                        pos,
                        size,
                        color: Color32::from_rgb(220, 38, 38),
                        text,
                    });
                    None
                }
            }
        };
    }

    fn effective_color(&self) -> Color32 {
        // 右列(透明色)= 马克笔效果,对所有绘图工具生效
        if self.opaque {
            self.color
        } else {
            with_alpha(self.color, 110)
        }
    }

    fn eraser_radius(&self) -> f32 {
        (self.width * 4.0).max(12.0)
    }

    fn drag_stroke(&mut self, pos: Pos2, shift: bool) {
        match &mut self.canvas.current {
            Some(Ink::Freehand { pts, .. }) => {
                if pts.last() != Some(&pos) {
                    pts.push(pos);
                }
            }
            Some(Ink::Line { a, b, .. } | Ink::Arrow { a, b, .. }) => {
                *b = snap_45(*a, pos, shift);
            }
            Some(Ink::Rect { a, b, .. } | Ink::Ellipse { a, b, .. }) => {
                *b = square_constrain(*a, pos, shift);
            }
            _ => {}
        }
    }

    // ---------- 框选截图 ----------

    fn region_input(&mut self, ctx: &Context, over_ui: bool, down: bool, pos: Option<Pos2>) {
        if over_ui {
            return; // 指针在工具条上,不打断当前选区
        }
        if down {
            if let Some(p) = pos {
                self.region_sel = Some(match self.region_sel.take() {
                    Some(mut s) => {
                        s.cur = p;
                        s
                    }
                    None => RegionSel { start: p, cur: p },
                });
            }
        } else if let Some(sel) = self.region_sel.take() {
            // 松开:只保留选区,不立即保存;之后「存图/复制」只作用于选区
            self.region_armed = false;
            let r = Rect::from_two_pos(sel.start, sel.cur);
            if r.width() >= 4.0 && r.height() >= 4.0 {
                self.region_sel = Some(RegionSel { start: r.min, cur: r.max });
                self.region_zoom = 1.0;
                self.zoom_base = None;
                self.region_clean = None;
                self.bg_pending = None;
                self.bg_pending_sel = None;
                self.bg_pending_since = None;
                self.debak_started = None;
                self.debak_frames = 0;
                save_region_state(self.region_sel, ctx.pixels_per_point());
                self.toast = Some((
                    self.lang.strings().region_selected.to_string(),
                    Instant::now(),
                ));
            }
        }
    }

    /// 当前选区换算成**窗口内**物理像素矩形(无选区返回 None)。
    /// 覆盖层铺满虚拟桌面,所以窗口内坐标 + 虚拟桌面原点 = 桌面物理坐标。
    fn target_region(&self, ctx: &Context) -> Option<Rect> {
        let sel = self.region_sel?;
        let ppp = ctx.pixels_per_point();
        let r = Rect::from_two_pos(sel.start, sel.cur);
        let (_, _, vw, vh) = self.screen;
        let x = (r.min.x * ppp).floor().max(0.0) as i32;
        let y = (r.min.y * ppp).floor().max(0.0) as i32;
        let w = ((r.width() * ppp).ceil() as i32).clamp(1, (vw - x).max(1));
        let h = ((r.height() * ppp).ceil() as i32).clamp(1, (vh - y).max(1));
        Some(Rect::from_min_size(
            Pos2::new(x as f32, y as f32),
            Vec2::new(w as f32, h as f32),
        ))
    }

    /// 窗口内物理像素矩形 → 虚拟桌面物理像素坐标(截屏用)
    fn to_desktop_px(&self, r: Rect) -> (i32, i32, i32, i32) {
        let (vx, vy, _, _) = self.screen;
        (
            r.left() as i32 + vx,
            r.top() as i32 + vy,
            r.width() as i32,
            r.height() as i32,
        )
    }

    // ---------- 放大镜 ----------

    fn magnifier_ui(&mut self, ctx: &Context, over_ui: bool, pos: Option<Pos2>) {
        ctx.request_repaint_after(Duration::from_millis(50));
        let Some(pos) = pos else { return };
        if over_ui {
            return; // 在工具条上时不显示,避免遮挡操作
        }

        // 滚轮调倍率
        let scroll = ctx.input(|i| i.smooth_scroll_delta().y);
        if scroll != 0.0 {
            self.zoom = (self.zoom + if scroll > 0.0 { 0.25 } else { -0.25 }).clamp(1.5, 6.0);
        }

        let ppp = ctx.pixels_per_point();
        let (vx, vy, vw, vh) = self.screen;
        let sample_w = ((140.0 * ppp).round() as i32).clamp(1, vw);
        let sample_h = ((105.0 * ppp).round() as i32).clamp(1, vh);
        // 指针是窗口内逻辑坐标 → 虚拟桌面物理坐标
        let cx = vx + (pos.x * ppp) as i32;
        let cy = vy + (pos.y * ppp) as i32;
        let sx = (cx - sample_w / 2).clamp(vx, (vx + vw - sample_w).max(vx));
        let sy = (cy - sample_h / 2).clamp(vy, (vy + vh - sample_h).max(vy));
        let Ok(shot) = capture::capture_region_px(sx, sy, sample_w, sample_h, self.lang) else {
            return;
        };
        let img = ColorImage::from_rgba_unmultiplied(
            [sample_w as usize, sample_h as usize],
            &shot.rgba,
        );
        let handle = match &mut self.mag_tex {
            Some(t) => {
                t.set(img, TextureOptions::NEAREST);
                t.clone()
            }
            None => {
                let t = ctx.load_texture("magnifier", img, TextureOptions::NEAREST);
                self.mag_tex = Some(t.clone());
                t
            }
        };

        let size = Vec2::new(140.0 * self.zoom, 105.0 * self.zoom);
        let screen = self.screen_logical(ctx);
        let (lw, lh) = (screen.x, screen.y);
        let mut min = pos + egui::vec2(90.0, 90.0);
        if min.x + size.x > lw - 4.0 {
            min.x = pos.x - 90.0 - size.x;
        }
        if min.y + size.y > lh - 4.0 {
            min.y = pos.y - 90.0 - size.y;
        }
        let rect = Rect::from_min_size(min, size);

        let painter = ctx.layer_painter(LayerId::new(Order::Foreground, Id::unique("magnifier")));
        painter.image(
            handle.id(),
            rect,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
        painter.rect_stroke(rect, 0.0, Stroke::new(2.0, Color32::WHITE), egui::epaint::StrokeKind::Inside);
        painter.rect_stroke(rect, 0.0, Stroke::new(1.0, Color32::BLACK), egui::epaint::StrokeKind::Outside);
        painter.text(
            rect.left_bottom() + egui::vec2(0.0, 16.0),
            Align2::LEFT_CENTER,
            format!("×{:.1}", self.zoom),
            FontId::proportional(13.0),
            Color32::WHITE,
        );
    }

    // ---------- 设置(语言 / 存图格式与清晰度 / 关于) ----------

    fn settings_ui(&mut self, ctx: &Context) {
        if !self.show_settings {
            return;
        }
        let tr = self.lang.strings();
        let screen = self.screen_logical(ctx);
        let (lw, lh) = (screen.x, screen.y);
        let mut open = true;
        let mut close = false;
        let mut lang_changed = false;
        let mut debug_changed = false;
        // 窗口关闭后再比对,判断存图/去背设置是否需要落盘
        let shot_before = self.shot;
        let bg_before = self.bg_method;
        let dark_before = self.dark_light;
        egui::Window::new(tr.settings_title)
            // 标题随语言变化,固定 Id 让窗口位置/状态不因切语言重置
            .id(Id::unique("settings"))
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .pivot(Align2::CENTER_CENTER)
            .default_pos(Pos2::new(lw / 2.0, lh / 2.0))
            .default_width(520.0)
            .show(ctx, |ui| {
                ui.set_min_width(500.0);
                ui.set_max_width(500.0);
                // 语言:选中后立即生效并持久化
                ui.horizontal(|ui| {
                    ui.label(tr.language_label);
                    egui::ComboBox::from_id_salt("settings_lang")
                        .selected_text(self.lang.native_name())
                        .show_ui(ui, |ui| {
                            for lang in Lang::ALL {
                                if ui
                                    .selectable_value(&mut self.lang, lang, lang.native_name())
                                    .changed()
                                {
                                    lang_changed = true;
                                }
                            }
                        });
                });
                ui.separator();
                // 存图格式与清晰度:改动即持久化(下次启动沿用)
                ui.label(egui::RichText::new(tr.shot_heading).strong());
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.label(tr.format_label);
                    egui::ComboBox::from_id_salt("settings_format")
                        .selected_text(match self.shot.format {
                            ShotFormat::Webp => tr.format_webp,
                            ShotFormat::Png => tr.format_png,
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.shot.format, ShotFormat::Webp, tr.format_webp);
                            ui.selectable_value(&mut self.shot.format, ShotFormat::Png, tr.format_png);
                        });
                });
                // 清晰度只对 WebP 有损编码有意义;PNG 与 WebP 无损都是画质不变的压缩
                let webp = self.shot.format == ShotFormat::Webp;
                let quality_enabled = webp && !self.shot.lossless;
                ui.horizontal(|ui| {
                    ui.add_enabled_ui(quality_enabled, |ui| {
                        ui.label(tr.quality_label);
                        ui.add(egui::Slider::new(&mut self.shot.quality, 1..=100));
                    });
                    ui.add_enabled_ui(webp, |ui| {
                        ui.checkbox(&mut self.shot.lossless, tr.lossless_checkbox);
                    });
                });
                ui.label(egui::RichText::new(tr.quality_tip).weak());
                ui.separator();
                // 选区去背:键控 / ISNet ONNX 模型 / 自动回退
                ui.label(egui::RichText::new(tr.bg_heading).strong());
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.label(tr.bg_method_label);
                    egui::ComboBox::from_id_salt("settings_bg_method")
                        .selected_text(match self.bg_method {
                            BgMethod::Auto => tr.bg_method_auto,
                            BgMethod::Keying => tr.bg_method_keying,
                            BgMethod::Isnet => tr.bg_method_isnet,
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.bg_method,
                                BgMethod::Auto,
                                tr.bg_method_auto,
                            );
                            ui.selectable_value(
                                &mut self.bg_method,
                                BgMethod::Keying,
                                tr.bg_method_keying,
                            );
                            ui.selectable_value(
                                &mut self.bg_method,
                                BgMethod::Isnet,
                                tr.bg_method_isnet,
                            );
                        });
                });
                match isnet::find_model() {
                    Some(path) => {
                        let name = path
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("isnet.onnx");
                        ui.label(
                            egui::RichText::new(i18n::fill(tr.bg_model_found, &[name]))
                                .weak(),
                        )
                        .on_hover_text(path.display().to_string());
                    }
                    None => {
                        ui.label(egui::RichText::new(tr.bg_model_missing).color(RED));
                    }
                }
                match isnet::find_dll() {
                    Some(_) => {
                        ui.label(egui::RichText::new(tr.bg_runtime_found).weak());
                    }
                    None => {
                        ui.label(egui::RichText::new(tr.bg_runtime_missing).color(RED));
                    }
                }
                ui.label(egui::RichText::new(tr.bg_model_tip).weak());
                let mut dark = self.dark_light;
                if ui.checkbox(&mut dark, tr.bg_dark_light_checkbox).changed() {
                    self.dark_light = dark;
                }
                ui.separator();
                // 签名章:改动即持久化(下次启动沿用)
                ui.label(egui::RichText::new(tr.signature_heading).strong());
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.label(tr.signature_label);
                    let resp = ui.add_sized(
                        [360.0, 24.0],
                        egui::TextEdit::singleline(&mut self.signature),
                    );
                    if resp.changed() {
                        self.signature = self.signature.replace('\n', " ");
                        self.save_state();
                    }
                });
                ui.label(
                    egui::RichText::new(tr.tool_signature).weak(),
                );
                ui.add_space(4.0);
                let mut dbg = self.debug_shot;
                if ui.checkbox(&mut dbg, tr.debug_checkbox).changed() {
                    self.debug_shot = dbg;
                    debug_changed = true;
                }
                ui.separator();
                ui.label(egui::RichText::new(tr.about_heading).strong());
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new(format!("PointToFix v{}", env!("CARGO_PKG_VERSION")))
                        .strong()
                        .size(18.0),
                );
                ui.label(tr.app_desc);
                ui.add_space(4.0);
                ui.label(egui::RichText::new(tr.shortcuts).weak());
                ui.add_space(2.0);
                ui.label(egui::RichText::new(tr.tips).weak());
                ui.add_space(6.0);
                if ui
                    .add_sized([ui.available_width(), 26.0], egui::Button::new(tr.close))
                    .clicked()
                {
                    close = true;
                }
            });
        if close || !open {
            self.show_settings = false;
        }
        if lang_changed
            || debug_changed
            || self.shot != shot_before
            || self.bg_method != bg_before
            || self.dark_light != dark_before
        {
            self.save_state();
        }
    }

    // ---------- 工具条 ----------

    fn toolbar_ui(&mut self, ctx: &Context) {
        let tr = self.lang.strings();
        // 覆盖层就是当前显示器,所以这里的逻辑尺寸既是窗口范围也是这块屏的范围
        let screen = self.screen_logical(ctx);
        let (lw, lh) = (screen.x, screen.y);
        // 屏幕太矮装不下整条工具条时等比缩小(尺寸随屏幕自适应,永远不跨屏)
        let scale = std::env::var("POINTTOFIX_BAR_SCALE")
            .ok()
            .and_then(|v| v.parse::<f32>().ok())
            .unwrap_or_else(|| ((lh - 10.0) / self.bar_unit_h.max(1.0)).clamp(0.4, 1.0));
        self.bar_scale = scale;
        let bar_w = BAR_WIDTH * scale;
        let bar_h = self.bar_unit_h * scale;
        let mut pos = self
            .bar_pos
            .unwrap_or_else(|| Pos2::new((lw - bar_w - 6.0).max(6.0), 6.0));
        // 位置整体限制在这块屏内(窗口不会跨屏,所以夹到窗口范围即可)
        pos.x = pos.x.clamp(4.0, (lw - bar_w - 4.0).max(4.0));
        pos.y = pos.y.clamp(4.0, (lh - bar_h - 4.0).max(4.0));
        let mut moved = false;
        let geom_log = std::env::var("POINTTOFIX_GEOM_LOG").is_ok();
        let win = egui::Window::new("toolbar")
            .title_bar(false)
            .resizable(false)
            .collapsible(false)
            .fixed_pos(pos)
            .frame(
                Frame::NONE
                    .fill(Color32::from_rgba_unmultiplied(
                        TOOLBAR_BG.0, TOOLBAR_BG.1, TOOLBAR_BG.2, TOOLBAR_BG.3,
                    ))
                    .corner_radius(egui::epaint::CornerRadius::same((6.0 * scale).round() as u8))
                    .inner_margin(egui::Margin::same((5.0 * scale).round() as i8))
                    .stroke(Stroke::new(1.0, STROKE_GRAY)),
            )
            .show(ctx, |ui| {
                ui.set_min_width((BAR_WIDTH - 10.0) * scale);
                ui.set_max_width((BAR_WIDTH - 10.0) * scale);
                // 文字与行距跟着缩放,否则缩小时文字会撑破按钮
                let style = ui.style_mut();
                for font in style.text_styles.values_mut() {
                    font.size *= scale;
                }
                style.spacing.item_spacing.y = 4.0 * scale;

                // 顶部拖动条:按住可移动工具条
                let grip = ui.allocate_response(
                    egui::vec2(ui.available_width(), 10.0 * scale),
                    egui::Sense::drag(),
                );
                if grip.hovered() {
                    ctx.set_cursor_icon(egui::CursorIcon::Grab);
                }
                if grip.dragged() {
                    let d = grip.drag_delta();
                    pos.x = (pos.x + d.x).clamp(4.0, (lw - bar_w - 4.0).max(4.0));
                    pos.y = (pos.y + d.y).clamp(4.0, (lh - bar_h - 4.0).max(4.0));
                    moved = true;
                }
                let gp = ui.painter();
                let center = grip.rect.center();
                for dx in [-8.0f32, 0.0, 8.0] {
                    gp.circle_filled(
                        center + egui::vec2(dx * scale, 0.0),
                        1.5 * scale,
                        Color32::from_rgb(150, 150, 155),
                    );
                }

                // 完成(原版 Fertig)
                if ui
                    .add_sized(
                        [ui.available_width(), 26.0 * scale],
                        egui::Button::new(egui::RichText::new(tr.done).color(DARK_TEXT))
                            .fill(Color32::WHITE)
                            .stroke(Stroke::new(1.0, STROKE_GRAY)),
                    )
                    .clicked()
                {
                    self.exit_active(ctx);
                }
                ui.add_space(2.0 * scale);

                // 笔宽:4 档圆点(原版样式)
                Self::icon_grid(ui, "row_width", scale, |ui| {
                    let sizes = [2.0f32, 4.0, 8.0, 12.0];
                    for (i, w) in sizes.iter().enumerate() {
                        let tip = i18n::fill(tr.pen_width_tip, &[&(*w as i32).to_string()]);
                        if self
                            .icon_btn(ui, Icon::Dot((i + 1) as u8), &tip, self.width == *w, false)
                            .clicked()
                        {
                            self.width = *w;
                        }
                        if i % 2 == 1 {
                            ui.end_row();
                        }
                    }
                });

                ui.separator();

                // 双色卡:左列画笔色,右列荧光笔色,点击即切换工具(原版 5×2)
                egui::Grid::new("colors")
                    .num_columns(2)
                    .min_col_width(38.0 * scale)
                    .spacing([4.0 * scale, 4.0 * scale])
                    .show(ui, |ui| {
                        for i in 0..5 {
                            self.color_swatch(ui, PEN_COLORS[i], false);
                            self.color_swatch(ui, HL_COLORS[i], true);
                            ui.end_row();
                        }
                    });

                ui.separator();

                // 工具与功能(每行两个,对应原版排布)
                Self::icon_grid(ui, "tools", scale, |ui| {
                    self.tool_icon(ui, Icon::Pen, tr.tool_pen, Tool::Pen);
                    self.tool_icon(ui, Icon::Eraser, tr.tool_eraser, Tool::Eraser);
                    ui.end_row();
                    self.tool_icon(ui, Icon::Line, tr.tool_line, Tool::Line);
                    self.tool_icon(ui, Icon::Arrow, tr.tool_arrow, Tool::Arrow);
                    ui.end_row();
                    self.tool_icon(ui, Icon::Rect, tr.tool_rect, Tool::Rect);
                    self.tool_icon(ui, Icon::RectFilled, tr.tool_rect_filled, Tool::RectFilled);
                    ui.end_row();
                    self.tool_icon(ui, Icon::Ellipse, tr.tool_ellipse, Tool::Ellipse);
                    self.tool_icon(ui, Icon::EllipseFilled, tr.tool_ellipse_filled, Tool::EllipseFilled);
                    ui.end_row();
                    self.tool_icon(ui, Icon::DoubleArrow, tr.tool_double_arrow, Tool::DoubleArrow);
                    self.tool_icon(ui, Icon::Text, tr.tool_text, Tool::Text);
                    ui.end_row();
                    self.tool_icon(ui, Icon::CheckStamp, tr.tool_check_stamp, Tool::CheckStamp);
                    self.tool_icon(ui, Icon::CrossStamp, tr.tool_cross_stamp, Tool::CrossStamp);
                    ui.end_row();
                    self.tool_icon(ui, Icon::TimeStamp, tr.tool_time_stamp, Tool::Time);
                    self.tool_icon(ui, Icon::Signature, tr.tool_signature, Tool::Signature);
                    ui.end_row();
                    if self
                        .icon_btn(ui, Icon::Region, tr.tool_region, self.region_armed, false)
                        .clicked()
                    {
                        self.region_armed = !self.region_armed;
                        self.region_sel = None;
                    }
                    if self
                        .icon_btn(
                            ui,
                            Icon::Debak,
                            tr.tool_debak,
                            self.region_clean.is_some(),
                            false,
                        )
                        .clicked()
                    {
                        self.toggle_region_clean(ctx);
                    }
                    ui.end_row();
                    if self
                        .icon_btn(ui, Icon::ZoomIn, tr.tool_zoom_in, false, false)
                        .clicked()
                    {
                        self.region_zoom_step(true);
                    }
                    if self
                        .icon_btn(ui, Icon::ZoomOut, tr.tool_zoom_out, false, false)
                        .clicked()
                    {
                        self.region_zoom_step(false);
                    }
                    ui.end_row();
                    if self.icon_btn(ui, Icon::UndoArrow, tr.tool_undo, false, false).clicked() {
                        self.canvas.undo();
                    }
                    if self
                        .icon_btn(ui, Icon::ClearAll, tr.tool_clear_all, false, true)
                        .clicked()
                    {
                        self.canvas.clear();
                    }
                    ui.end_row();
                    if self.icon_btn(ui, Icon::Print, tr.tool_print, false, false).clicked() {
                        self.do_print(ctx);
                    }
                    if self
                        .icon_btn(ui, Icon::Save, tr.tool_save, false, false)
                        .clicked()
                    {
                        self.do_save(ctx);
                    }
                    ui.end_row();
                    if self
                        .icon_btn(ui, Icon::Copy, tr.tool_copy, false, false)
                        .clicked()
                    {
                        self.do_copy(ctx);
                    }
                    if self
                        .icon_btn(ui, Icon::Settings, tr.tool_settings, self.show_settings, false)
                        .clicked()
                    {
                        self.show_settings = !self.show_settings;
                    }
                    ui.end_row();
                });
            });
        // 实测本帧工具条高度,换算回缩放 1.0 供下一帧决定缩放与位置(线性缩放假设)
        if let Some(win) = &win {
            let unit_h = win.response.rect.height() / self.bar_scale.max(0.01);
            if unit_h.is_finite() && unit_h > 1.0 {
                self.bar_unit_h = unit_h;
            }
        }
        if geom_log && (self.geom_log_frames <= 10 || self.geom_log_frames % 120 == 1) {
            if let Some(win) = &win {
                eprintln!(
                    "[toolbar] pos={:?} rect={:?} scale={:.2} unit_h={:.1} viewport={:?}",
                    pos,
                    win.response.rect,
                    self.bar_scale,
                    self.bar_unit_h,
                    ctx.input(|i| i.viewport_rect()),
                );
            }
        }
        if moved {
            self.bar_pos = Some(pos);
            self.save_state();
        }
    }

    fn icon_grid(ui: &mut egui::Ui, id: &str, scale: f32, add_row: impl FnOnce(&mut egui::Ui)) {
        egui::Grid::new(id)
            .num_columns(2)
            .min_col_width(38.0 * scale)
            .spacing([4.0 * scale, 4.0 * scale])
            .show(ui, |ui| add_row(ui));
    }

    fn icon_btn(
        &self,
        ui: &mut egui::Ui,
        icon: Icon,
        tip: &str,
        selected: bool,
        danger: bool,
    ) -> egui::Response {
        let s = self.bar_scale;
        let radius = egui::epaint::CornerRadius::same((4.0 * s).round().max(1.0) as u8);
        let (rect, resp) =
            ui.allocate_exact_size(egui::vec2(38.0 * s, 28.0 * s), egui::Sense::click());
        let p = ui.painter();
        let bg = if selected {
            SELECT_FILL
        } else if resp.hovered() {
            HOVER_FILL
        } else {
            Color32::WHITE
        };
        p.rect_filled(rect, radius, bg);
        p.rect_stroke(rect, radius, Stroke::new(1.0, STROKE_GRAY), egui::epaint::StrokeKind::Inside);
        icons::draw(
            p,
            icon,
            rect.shrink(4.5 * s),
            if danger { RED } else { DARK_TEXT },
            bg,
        );
        resp.on_hover_text(tip)
    }

    fn tool_icon(&mut self, ui: &mut egui::Ui, icon: Icon, tip: &str, tool: Tool) {
        if self.icon_btn(ui, icon, tip, self.tool == tool, false).clicked() {
            self.tool = tool;
        }
    }

    fn color_swatch(&mut self, ui: &mut egui::Ui, color: Color32, transparent: bool) {
        let tr = self.lang.strings();
        let s = self.bar_scale;
        let radius = egui::epaint::CornerRadius::same((3.0 * s).round().max(1.0) as u8);
        let (rect, resp) =
            ui.allocate_exact_size(egui::vec2(38.0 * s, 20.0 * s), egui::Sense::click());
        let p = ui.painter();
        p.rect_filled(rect.shrink(1.0 * s), radius, color);
        let is_current = if transparent {
            !self.opaque && self.color == color
        } else {
            self.opaque && self.color == color
        };
        if is_current {
            p.rect_stroke(
                rect,
                egui::epaint::CornerRadius::same(3),
                Stroke::new(2.0, STROKE_GRAY),
                egui::epaint::StrokeKind::Inside,
            );
        }
        if resp.clicked() {
            self.color = color;
            self.opaque = !transparent;
        }
        resp.on_hover_text(if transparent {
            tr.color_transparent_tip
        } else {
            tr.color_opaque_tip
        });
    }

    /// 选区去背开关(⊠ 按钮 / Ctrl+D):隐藏覆盖层等画面稳定后抓选区处理(图层隔离)
    fn toggle_region_clean(&mut self, ctx: &Context) {
        let tr = self.lang.strings();
        if self.region_sel.is_none() {
            self.set_toast(Err(tr.select_region_first.to_string()));
        } else if self.bg_pending.is_some() {
            // 后台还在处理:再点不重复提交
            self.set_toast(Ok(tr.debak_processing.to_string()));
        } else if self.region_clean.is_some() {
            self.region_clean = None;
            self.set_toast(Ok(tr.debak_restored.to_string()));
        } else {
            // 抓屏会连带覆盖层(选框线/标注)一起抓下来。窗口是透明置顶层,egui 画完到 DWM
            // 真正合成上屏有延迟(实测 1~4 帧),所以隐藏覆盖层后要等画面稳定再抓:
            // 至少再画 3 帧干净画面,并且经过 200ms,然后帧末抓屏。
            self.debak_started = Some(Instant::now());
            self.debak_frames = 2;
            ctx.request_repaint();
        }
    }

    /// 抓取(已隐藏标注的)选区画面,交给后台线程去背(ISNet 推理不能阻塞 UI)
    fn apply_debak_now(&mut self, ctx: &Context) -> Result<String, String> {
        let tr = self.lang.strings();
        let Some(r) = self.target_region(ctx) else {
            return Err(tr.select_region_first.to_string());
        };
        // 保险:内缩几像素,把选框内侧的白描边(+抗锯齿)排除在背景种子之外。
        // 正常等待后抓到的画面已无覆盖层,但 DWM 合成再慢时这层白边会主导边框直方图。
        let pad = (ctx.pixels_per_point().ceil() as i32 + 2).max(2);
        let (mut x, mut y) = (r.left() as i32, r.top() as i32);
        let (mut w, mut h) = (r.width() as i32, r.height() as i32);
        if w > pad * 3 && h > pad * 3 {
            x += pad;
            y += pad;
            w -= pad * 2;
            h -= pad * 2;
        }
        capture::flush_composition();
        let (vx, vy, _, _) = self.screen;
        let shot = capture::capture_region_px(vx + x, vy + y, w, h, self.lang)?;
        let Some(id) = bgremove::submit(shot, self.bg_method, self.dark_light) else {
            return Err("无法启动后台去背线程".to_string());
        };
        self.bg_pending = Some(id);
        self.bg_pending_sel = self.region_sel;
        self.bg_pending_since = Some(Instant::now());
        ctx.request_repaint();
        Ok(tr.debak_processing.to_string())
    }

    /// 轮询后台去背结果:成功后替换选区显示;失败/回退给出提示
    fn poll_bg_job(&mut self, ctx: &Context) {
        // 可能积压了过期任务(退出标注/换选区),一直取到当前任务为止
        let mut done = None;
        while let Some(result) = bgremove::poll() {
            if self.bg_pending == Some(result.id) {
                done = Some(result);
                break;
            }
        }
        if self.bg_pending.is_some() {
            // 还没完成(或刚丢弃过期结果):保持轮询,处理中提示也要走秒
            ctx.request_repaint_after(Duration::from_millis(80));
        }
        let Some(done) = done else { return };
        self.bg_pending = None;
        self.bg_pending_since = None;
        let pending_sel = self.bg_pending_sel.take();
        let tr = self.lang.strings();
        let issue_text = bg_issue_text(tr, done.issue.as_ref());
        // 自动化排查用:POINTTOFIX_DEBACK_LOG=1 打印去背引擎/耗时/错误
        if std::env::var("POINTTOFIX_DEBACK_LOG").is_ok() {
            eprintln!(
                "[deback] id={} engine={:?} size={}x{} took={:.1}s issue={}",
                done.id,
                done.engine,
                done.width,
                done.height,
                done.elapsed.as_secs_f32(),
                issue_text
            );
        }
        match done.rgba {
            Some(rgba) => {
                // 等待期间选区被改掉/清掉:这份结果已经对不上当前画面,丢弃
                if pending_sel.is_some() && pending_sel == self.region_sel {
                    let img = ColorImage::from_rgba_unmultiplied(
                        [done.width as usize, done.height as usize],
                        &rgba,
                    );
                    match &mut self.region_clean {
                        Some(t) => t.set(img, TextureOptions::NEAREST),
                        None => {
                            self.region_clean = Some(ctx.load_texture(
                                "region_clean",
                                img,
                                TextureOptions::NEAREST,
                            ));
                        }
                    }
                    let msg = if done.engine == BgEngine::Keying
                        && done.issue.is_some()
                    {
                        i18n::fill(tr.debak_ai_fallback, &[&issue_text])
                    } else {
                        tr.debak_applied.to_string()
                    };
                    self.set_toast(Ok(msg));
                }
            }
            None => self.set_toast(Err(issue_text)),
        }
        ctx.request_repaint();
    }

    /// 选框区域缩放:×1 → ×2 → ×4 → ×8 → ×10
    fn region_zoom_step(&mut self, zoom_in: bool) {
        const STEPS: [f32; 5] = [1.0, 2.0, 4.0, 8.0, 10.0];
        if self.region_sel.is_none() {
            self.set_toast(Err(self.lang.strings().select_region_first_zoom.to_string()));
            return;
        }
        let cur = STEPS
            .iter()
            .position(|z| (*z - self.region_zoom).abs() < 0.01)
            .unwrap_or(0);
        let next = if zoom_in {
            (cur + 1).min(STEPS.len() - 1)
        } else {
            cur.saturating_sub(1)
        };
        let from_normal = self.region_zoom <= 1.0;
        self.region_zoom = STEPS[next];
        if self.region_zoom > 1.0 && from_normal {
            // 刚从 1× 进入放大:下一帧抓一张没有缩放层的干净底图(冻结画面,不产生镜中镜)
            self.zoom_dirty = true;
        }
        if self.region_zoom <= 1.0 {
            self.zoom_base = None;
            self.zoom_dirty = false;
        }
    }

    /// 把选框区域的内容按倍率放大显示在框内(冻结底图,中心锚定,裁剪到框)
    fn region_zoom_ui(&mut self, ctx: &Context) {
        let Some(sel) = self.region_sel else {
            self.zoom_base = None;
            return;
        };
        if self.region_zoom <= 1.0 {
            return;
        }
        let r = Rect::from_two_pos(sel.start, sel.cur);
        if self.zoom_dirty || self.zoom_base.is_none() {
            // 抓底图(此刻上一帧还没画缩放层,是干净画面)
            let Some(tr) = self.target_region(ctx) else { return };
            let (dx, dy, w, h) = self.to_desktop_px(tr);
            let Ok(shot) = capture::capture_region_px(dx, dy, w, h, self.lang) else {
                return;
            };
            // 用实际抓到的尺寸(边缘可能被裁),避免纹理长度与声明尺寸对不上
            let img = ColorImage::from_rgba_unmultiplied(
                [shot.width as usize, shot.height as usize],
                &shot.rgba,
            );
            match &mut self.zoom_base {
                Some(t) => t.set(img, TextureOptions::NEAREST),
                None => {
                    self.zoom_base =
                        Some(ctx.load_texture("region_zoom_base", img, TextureOptions::NEAREST));
                }
            }
            self.zoom_dirty = false;
        }
        let Some(tex) = &self.zoom_base else { return };

        // 内容层:裁剪到框内,中心锚定放大
        let clip = ctx.layer_painter(LayerId::new(Order::Middle, Id::unique("region_zoom")));
        let painter = clip.with_clip_rect(r);
        let big = Rect::from_center_size(r.center(), r.size() * self.region_zoom);
        painter.image(
            tex.id(),
            big,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
        // 边框 + 倍率标签
        let frame = ctx.layer_painter(LayerId::new(Order::Middle, Id::unique("region_zoom_frame")));
        frame.rect_stroke(r, 0.0, Stroke::new(2.0, Color32::BLACK), egui::epaint::StrokeKind::Outside);
        frame.rect_stroke(r, 0.0, Stroke::new(1.0, Color32::WHITE), egui::epaint::StrokeKind::Inside);
        frame.text(
            r.right_top() + egui::vec2(-2.0, -6.0),
            Align2::RIGHT_BOTTOM,
            format!("×{:.0}", self.region_zoom),
            FontId::proportional(14.0),
            Color32::WHITE,
        );
    }

    // ---------- 文字工具 ----------

    fn text_edit_ui(&mut self, ctx: &Context) {
        if self.text_edit.is_none() {
            return;
        }
        let mut committed: Option<(Pos2, String)> = None;
        egui::Window::new("text_edit")
            .title_bar(false)
            .resizable(false)
            .collapsible(false)
            .fixed_pos(self.text_edit.as_ref().unwrap().pos + egui::vec2(10.0, 10.0))
            .show(ctx, |ui| {
                let te = self.text_edit.as_mut().unwrap();
                let resp = ui.add_sized([240.0, 28.0], egui::TextEdit::singleline(&mut te.buf));
                if !te.focused {
                    te.focused = true;
                    resp.request_focus();
                }
                if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    committed = Some((te.pos, te.buf.clone()));
                }
            });
        if let Some((pos, text)) = committed {
            if !text.trim().is_empty() {
                let size = (self.width * 5.0).clamp(18.0, 72.0);
                self.canvas.shapes.push(Ink::Text {
                    pos,
                    text,
                    size,
                    color: self.effective_color(),
                });
            }
            self.text_edit = None;
        }
    }

    // ---------- 导出 ----------

    fn do_save(&mut self, ctx: &Context) {
        self.start_shot(ShotJob::Save, ctx);
    }

    fn do_copy(&mut self, ctx: &Context) {
        self.start_shot(ShotJob::Copy, ctx);
    }

    fn do_print(&mut self, ctx: &Context) {
        self.start_shot(ShotJob::Print, ctx);
    }

    /// 启动一次截图:底图 BitBlt + egui 帧缓冲合成(含墨迹;调试模式含工具条)
    fn start_shot(&mut self, job: ShotJob, ctx: &Context) {
        if self.shot_phase != ShotPhase::Idle {
            return; // 上一张还没处理完
        }
        self.shot_job = Some(job);
        self.shot_phase = ShotPhase::NeedFrame;
        ctx.request_repaint();
    }

    fn poll_shot(&mut self, ctx: &Context) {
        let taken = self.shot_slot.lock().ok().and_then(|mut s| s.take());
        let Some((job, overlay)) = taken else { return };
        self.shot_phase = ShotPhase::Idle;
        self.shot_job = None;
        let had_region = self.region_sel.is_some();
        let result = self.compose_and_dispatch(ctx, job, overlay, had_region);
        self.set_toast(result);
    }

    fn compose_and_dispatch(
        &mut self,
        ctx: &Context,
        job: ShotJob,
        overlay: Arc<ColorImage>,
        had_region: bool,
    ) -> Result<String, String> {
        // 裁剪区域(物理像素):有选区时只处理选区像素,避免整屏 4M 像素运算(否则 UI 明显卡顿)
        let crop = if self.debug_shot {
            None
        } else {
            self.target_region(ctx).map(|r| {
                (
                    (r.left() as i32).max(0),
                    (r.top() as i32).max(0),
                    (r.width() as i32).max(1),
                    (r.height() as i32).max(1),
                )
            })
        };
        let tr = self.lang.strings();
        // 1) 桌面底图(只抓需要的区域;crop 是窗口内坐标,截屏要换成桌面坐标)
        //    无选区 = 只抓当前这块显示器(覆盖层窗口的范围),不碰其他屏幕
        let (mx, my, mw, mh) = self.screen;
        let base = match crop {
            Some((x, y, w, h)) => capture::capture_region_px(mx + x, my + y, w, h, self.lang)?,
            None => capture::capture_region_px(mx, my, mw, mh, self.lang)?,
        };
        let bw = base.width as usize;
        let bh = base.height as usize;
        // 2) 覆盖层合成(Color32 为预乘 alpha:out = src + dst*(1-a));区域任务仅遍历区域像素
        let (ow, oh) = (overlay.size[0], overlay.size[1]);
        let (ox, oy) = match crop {
            Some((x, y, _, _)) => (x as usize, y as usize),
            None => (0, 0),
        };
        let mut rgba = base.rgba;
        let cw = bw.min(ow.saturating_sub(ox));
        let ch = bh.min(oh.saturating_sub(oy));
        for y in 0..ch {
            for x in 0..cw {
                let p = overlay.pixels[(oy + y) * ow + (ox + x)];
                let a = p.a() as u32;
                if a == 0 {
                    continue;
                }
                let inv = 255 - a;
                let i = (y * bw + x) * 4;
                rgba[i] = (p.r() as u32 + (rgba[i] as u32 * inv) / 255) as u8;
                rgba[i + 1] = (p.g() as u32 + (rgba[i + 1] as u32 * inv) / 255) as u8;
                rgba[i + 2] = (p.b() as u32 + (rgba[i + 2] as u32 * inv) / 255) as u8;
                rgba[i + 3] = 255;
            }
        }
        let shot = capture::Shot {
            width: bw as i32,
            height: bh as i32,
            rgba,
        };
        // 3) 分发(base 已是目标区域,无需再裁剪)
        let prefix = if had_region { tr.region_prefix } else { "" };
        match job {
            ShotJob::Save => {
                let (path, size) = save_shot_image(&shot, self.shot)?;
                let detail = format!("{} ({})", path.display(), human_size(size));
                Ok(i18n::fill(tr.saved_tip, &[&detail]))
            }
            ShotJob::Copy => {
                let (w, h) = (shot.width as usize, shot.height as usize);
                arboard::Clipboard::new()
                    .and_then(|mut cb| {
                        cb.set_image(arboard::ImageData {
                            width: w,
                            height: h,
                            bytes: std::borrow::Cow::Owned(shot.rgba),
                        })
                    })
                    .map_err(|e| e.to_string())?;
                Ok(format!("{prefix}{}", tr.copied_tip))
            }
            ShotJob::Print => {
                printing::print_rgba(&shot.rgba, shot.width, shot.height, self.lang)?;
                Ok(format!("{prefix}{}", tr.printed_tip))
            }
        }
    }

    fn set_toast(&mut self, result: Result<String, String>) {
        let msg = match result {
            Ok(m) => m,
            Err(e) => i18n::fill(self.lang.strings().failed_tip, &[&e]),
        };
        if std::env::var("POINTTOFIX_GEOM_LOG").is_ok() {
            eprintln!("[toast] {msg}");
        }
        self.toast = Some((msg, Instant::now()));
    }

    /// 帧末光标修正:active_ui 里设置的光标会被之后画的 egui::Window(工具条/设置/toast)
    /// 的 hover 光标覆盖,Window 之外的区域 egui 不会自动恢复。
    /// 在 ui() 末尾(所有窗口都画完了)按最终状态再写一次。
    fn post_ui_cursor_fix(&mut self, ctx: &Context) {
        let text_cursor = self.text_edit.is_some()
            || (self.tool == Tool::Text
                && self.mode == Mode::Active
                && !ctx.is_pointer_over_egui());
        ctx.output_mut(|o| {
            o.cursor_icon = if text_cursor {
                CursorIcon::Text
            } else {
                CursorIcon::Crosshair
            }
        });
    }

    /// 持久化界面状态(位置 / 调试模式 / 语言 / 存图格式与清晰度 / 去背方式)
    fn save_state(&self) {
        save_ui_state(
            self.standby_px,
            self.bar_pos,
            self.debug_shot,
            self.lang,
            self.shot,
            self.bg_method,
            self.dark_light,
            &self.signature,
        );
    }

    fn toast_ui(&mut self, ctx: &Context) {
        let Some((msg, at)) = self.toast.clone() else { return };
        if at.elapsed() > Duration::from_secs(3) {
            self.toast = None;
            return;
        }
        ctx.request_repaint_after(Duration::from_millis(200));
        let screen = self.screen_logical(ctx);
        let (lw, lh) = (screen.x, screen.y);
        egui::Window::new("toast")
            .title_bar(false)
            .resizable(false)
            .collapsible(false)
            .fixed_pos(Pos2::new((lw - 280.0) / 2.0, lh - 80.0))
            .show(ctx, |ui| {
                ui.set_min_width(280.0);
                ui.set_max_width(560.0); // 长语种提示自动换行,避免超宽窗口
                ui.label(msg);
            });
    }
}

/// 后台去背问题 → 界面文案(模型缺失走本地化提示,技术错误原样显示)
fn bg_issue_text(tr: &i18n::Strings, issue: Option<&BgIssue>) -> String {
    match issue {
        Some(BgIssue::ModelMissing) => tr.bg_model_missing.to_string(),
        Some(BgIssue::Error(e)) => e.clone(),
        None => String::new(),
    }
}

/// Shift 约束:直线/箭头吸附到 45° 方向
fn snap_45(origin: Pos2, pos: Pos2, shift: bool) -> Pos2 {
    if !shift {
        return pos;
    }
    let d = pos - origin;
    let len = d.length();
    if len < 0.001 {
        return pos;
    }
    let step = std::f32::consts::FRAC_PI_4;
    let angle = (d.y.atan2(d.x) / step).round() * step;
    origin + Vec2::new(angle.cos(), angle.sin()) * len
}

/// Shift 约束:矩形→正方形、椭圆→正圆
fn square_constrain(origin: Pos2, pos: Pos2, shift: bool) -> Pos2 {
    if !shift {
        return pos;
    }
    let d = pos - origin;
    let m = d.x.abs().min(d.y.abs());
    origin + Vec2::new(d.x.signum() * m, d.y.signum() * m)
}

/// 选区状态的持久化路径(%APPDATA%\pointtofix\region.txt)
fn region_state_path() -> std::path::PathBuf {
    std::env::var("APPDATA")
        .map(|p| std::path::PathBuf::from(p).join("pointtofix").join("region.txt"))
        .unwrap_or_else(|_| std::path::PathBuf::from("pointtofix_region.txt"))
}

/// 待机面板 / 工具条位置的持久化路径
fn ui_state_path() -> std::path::PathBuf {
    std::env::var("APPDATA")
        .map(|p| std::path::PathBuf::from(p).join("pointtofix").join("ui.txt"))
        .unwrap_or_else(|_| std::path::PathBuf::from("pointtofix_ui.txt"))
}

fn parse_pos(s: &str) -> Option<Pos2> {
    let mut it = s.split(',');
    let x: f32 = it.next()?.trim().parse().ok()?;
    let y: f32 = it.next()?.trim().parse().ok()?;
    if x.is_finite() && y.is_finite() {
        Some(Pos2::new(x, y))
    } else {
        None
    }
}

/// 读取界面状态。`standby` 是待机面板的**物理像素**位置(跨 DPI 不漂),
/// `toolbar` 是标注层内逻辑坐标(图层本身铺满虚拟桌面,逻辑坐标已足够)
fn load_ui_state() -> (
    Option<Pos2>,
    Option<Pos2>,
    bool,
    Lang,
    ShotSettings,
    BgMethod,
    bool,
    String,
) {
    let mut standby = None;
    let mut toolbar = None;
    let mut debug = false;
    let mut lang = None;
    let mut shot = ShotSettings::default();
    let mut bg_method = BgMethod::default();
    let mut dark_light = true;
    let mut signature = String::new();
    if let Ok(text) = std::fs::read_to_string(ui_state_path()) {
        // 用记事本/PowerShell 改过这个文件时可能带 UTF-8 BOM,会让第一行的键名匹配失败
        for line in text.trim_start_matches('\u{feff}').lines() {
            let line = line.trim();
            // 键名带 _px:旧版的 standby= 存的是逻辑坐标,语义不同,不能直接按物理像素用
            if let Some(v) = line.strip_prefix("standby_px=") {
                standby = parse_pos(v);
            } else if let Some(v) = line.strip_prefix("toolbar=") {
                toolbar = parse_pos(v);
            } else if let Some(v) = line.strip_prefix("lang=") {
                lang = Lang::from_code(v.trim());
            } else if line == "debug=1" {
                debug = true;
            } else if let Some(v) = line.strip_prefix("format=") {
                if let Some(f) = ShotFormat::from_code(v.trim()) {
                    shot.format = f;
                }
            } else if let Some(v) = line.strip_prefix("quality=") {
                if let Ok(q) = v.trim().parse::<u8>() {
                    shot.quality = q.clamp(1, 100);
                }
            } else if line == "lossless=1" {
                shot.lossless = true;
            } else if let Some(v) = line.strip_prefix("bg_method=") {
                if let Some(m) = BgMethod::from_code(v) {
                    bg_method = m;
                }
            } else if line == "dark_light=0" {
                dark_light = false;
            } else if let Some(v) = line.strip_prefix("signature=") {
                // 签名整行取值(可含空格);换行符是 TextEdit 不该出现的脏数据,忽略
                signature = v.trim_end().trim_start().to_string();
            }
        }
    }
    (
        standby,
        toolbar,
        debug,
        lang.unwrap_or_else(Lang::detect),
        shot,
        bg_method,
        dark_light,
        signature,
    )
}

fn save_ui_state(
    standby: Option<Pos2>,
    toolbar: Option<Pos2>,
    debug: bool,
    lang: Lang,
    shot: ShotSettings,
    bg_method: BgMethod,
    dark_light: bool,
    signature: &str,
) {
    let path = ui_state_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let mut text = String::new();
    if let Some(p) = standby {
        text.push_str(&format!("standby_px={:.1},{:.1}\n", p.x, p.y));
    }
    if let Some(p) = toolbar {
        text.push_str(&format!("toolbar={:.1},{:.1}\n", p.x, p.y));
    }
    if debug {
        text.push_str("debug=1\n");
    }
    text.push_str(&format!("lang={}\n", lang.code()));
    text.push_str(&format!("format={}\n", shot.format.code()));
    text.push_str(&format!("quality={}\n", shot.quality));
    if shot.lossless {
        text.push_str("lossless=1\n");
    }
    text.push_str(&format!("bg_method={}\n", bg_method.code()));
    if !dark_light {
        text.push_str("dark_light=0\n");
    }
    if !signature.is_empty() {
        // 签名放最后一行整行存:内容可含空格,不参与键值冲突
        text.push_str(&format!("signature={}\n", signature.replace('\n', " ")));
    }
    let _ = std::fs::write(&path, text);
}

/// 选区状态保存为**物理像素**:只存逻辑坐标的话,换到不同 DPI 的显示器再打开会整体缩放漂移
fn save_region_state(sel: Option<RegionSel>, ppp: f32) {
    let path = region_state_path();
    match sel {
        Some(sel) => {
            let ppp = ppp.max(0.1);
            let r = Rect::from_two_pos(sel.start, sel.cur);
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(
                &path,
                format!(
                    "{},{},{},{}",
                    r.min.x * ppp,
                    r.min.y * ppp,
                    r.width() * ppp,
                    r.height() * ppp
                ),
            );
        }
        None => {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// 读取保存的选区,按当前窗口 DPI 换算回窗口内逻辑坐标
fn load_saved_region(ppp: f32) -> Option<(f32, f32, f32, f32)> {
    let text = std::fs::read_to_string(region_state_path()).ok()?;
    let vals: Vec<f32> = text
        .trim_start_matches('\u{feff}')
        .trim()
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect();
    let ppp = ppp.max(0.1);
    if vals.len() == 4 {
        let (x, y, w, h) = (vals[0] / ppp, vals[1] / ppp, vals[2] / ppp, vals[3] / ppp);
        if w > 4.0 && h > 4.0 {
            return Some((x, y, w, h));
        }
    }
    None
}

/// 按设置编码截图,返回(扩展名, 字节流)。纯函数,便于单元测试
fn encode_shot(shot: &capture::Shot, set: ShotSettings) -> Result<(&'static str, Vec<u8>), String> {
    let (w, h) = (shot.width as u32, shot.height as u32);
    match set.format {
        ShotFormat::Png => {
            let mut buf = Vec::new();
            image::codecs::png::PngEncoder::new(&mut buf)
                .write_image(&shot.rgba, w, h, image::ExtendedColorType::Rgba8)
                .map_err(|e| e.to_string())?;
            Ok(("png", buf))
        }
        ShotFormat::Webp => {
            let enc = webp::Encoder::from_rgba(&shot.rgba, w, h);
            let mem = if set.lossless {
                enc.encode_lossless()
            } else {
                enc.encode(set.quality as f32)
            };
            Ok(("webp", mem.to_vec()))
        }
    }
}

/// 存一张截图到桌面,返回文件路径与文件大小(字节)
fn save_shot_image(shot: &capture::Shot, set: ShotSettings) -> Result<(std::path::PathBuf, u64), String> {
    let dir = std::env::var("USERPROFILE")
        .map(|p| std::path::PathBuf::from(p).join("Desktop"))
        .unwrap_or_else(|_| std::path::PathBuf::from("."));
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let (ext, bytes) = encode_shot(shot, set)?;
    let path = dir.join(format!("pointtofix_{ts}.{ext}"));
    std::fs::write(&path, &bytes).map_err(|e| e.to_string())?;
    Ok((path, bytes.len() as u64))
}

/// 时间章文本:点击那刻的本地日期+整点。中日文用"…23时"式,其余用"2026-10-30 23:00"式
fn local_time_stamp_text(lang: Lang) -> String {
    // GetLocalTime 只是读时钟,无失败路径
    let st = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    let (y, mo, d, h) = (st.wYear, st.wMonth, st.wDay, st.wHour);
    match lang {
        Lang::Zh | Lang::Ja => format!("{y}年{mo}月{d}日 {h:02}时"),
        Lang::En | Lang::De => format!("{y:04}-{mo:02}-{d:02} {h:02}:00"),
    }
}

/// 文件大小的人类可读形式(提示消息里用)
fn human_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if b < KB {
        format!("{bytes} B")
    } else if b < KB * KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{:.2} MB", b / (KB * KB))
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        // 上一帧点了「开始标注」并把面板藏了起来:现在抓底图、铺屏、进入标注
        if self.pending_entry {
            self.enter_active(&ctx);
        }
        // 首帧:优先按环境变量直接进标注模式(便于自动化测试),否则摆到屏幕右上角
        if !self.placed {
            self.placed = true;
            if std::env::var("POINTTOFIX_START_ACTIVE").is_ok() {
                self.begin_enter_active(&ctx);
            } else if let Some(p) = self.standby_px {
                let ppp = ctx.pixels_per_point();
                self.move_window_px(&ctx, self.clamp_standby_px(p, ppp));
            } else {
                let ppp = ctx.pixels_per_point();
                let (w, _) = capture::screen_size();
                let x = (w as f32 - (STANDBY_SIZE.x + 8.0) * ppp).max(8.0 * ppp);
                self.move_window_px(&ctx, Pos2::new(x, 8.0 * ppp));
            }
        }
        // 显示器热插拔 / 改分辨率 / 改缩放:刷新布局,标注层重新铺满目标显示器
        self.sync_screen_layout(&ctx);
        // 多显示器排查用:POINTTOFIX_GEOM_LOG=1 时打印前 10 帧、之后每 120 帧的视口/窗口实际几何
        if std::env::var("POINTTOFIX_GEOM_LOG").is_ok() {
            self.geom_log_frames += 1;
            if self.geom_log_frames <= 10 || self.geom_log_frames % 120 == 1 {
                let vp = ctx.input(|i| i.viewport().clone());
                let actual = self.hwnd.and_then(|h| {
                    let mut r = windows::Win32::Foundation::RECT::default();
                    unsafe { GetWindowRect(h, &mut r).ok()? };
                    Some((r.left, r.top, r.right - r.left, r.bottom - r.top))
                });
                eprintln!(
                    "[geom] mode={:?} ppp={:.3} screen={:?} vp_inner={:?} vp_outer={:?} window={actual:?} bar={:?}",
                    self.mode,
                    ctx.pixels_per_point(),
                    ctx.input(|i| i.viewport_rect()),
                    vp.inner_rect,
                    vp.outer_rect,
                    self.bar_pos,
                );
            }
        }
        // 截图/去背帧:隐藏界面元素;去背期间额外隐藏标注层(图层隔离,标注不进背景处理)
        let debaking = self.debak_started.is_some();
        self.shot_skip_ink = debaking;
        self.shot_skip_chrome =
            (self.shot_phase == ShotPhase::NeedFrame && !self.debug_shot) || debaking;
        self.poll_shot(&ctx);
        self.poll_bg_job(&ctx);
        match self.mode {
            Mode::Standby => self.standby_ui(&ctx, ui),
            Mode::Active => self.active_ui(&ctx),
        }
        if !self.shot_skip_chrome {
            self.toast_ui(&ctx);
        }
        // 本帧渲染完后再请求帧缓冲截图(eFrame 在本帧绘制末端读像素,含透明通道)
        if self.shot_phase == ShotPhase::NeedFrame {
            if let Some(job) = self.shot_job {
                let slot = self.shot_slot.clone();
                let ctx2 = ctx.clone();
                ctx.send_viewport_cmd(ViewportCommand::Screenshot(egui::ScreenshotCallback::new(
                    move |image| {
                        if let Ok(mut s) = slot.lock() {
                            *s = Some((job, image));
                        }
                        ctx2.request_repaint();
                    },
                )));
                self.shot_phase = ShotPhase::Sent;
                self.shot_sent_wait = 0;
            } else {
                self.shot_phase = ShotPhase::Idle;
            }
        } else if self.shot_phase == ShotPhase::Sent {
            // 窗口尺寸变化等情况下截图请求可能被吞:超时后重新请求
            self.shot_sent_wait += 1;
            ctx.request_repaint();
            if self.shot_sent_wait > 30 {
                self.shot_phase = ShotPhase::NeedFrame;
                self.shot_sent_wait = 0;
            }
        }
        // 去背:覆盖层已隐藏若干帧且画面稳定,此时抓屏不会抓到选框/标注
        if let Some(started) = self.debak_started {
            ctx.request_repaint();
            self.debak_frames = self.debak_frames.saturating_sub(1);
            let settled = self.debak_frames == 0
                && started.elapsed() >= Duration::from_millis(200);
            if settled {
                self.debak_started = None;
                let result = self.apply_debak_now(&ctx);
                self.set_toast(result);
            }
        }
        // 帧末光标修正:工具条等 Window 内部的 hover 光标只应在 Window 上生效
        self.post_ui_cursor_fix(&ctx);
    }

    // 透明窗:清屏必须全透明,否则整屏会蒙一层 eframe 默认的半透明黑
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        egui::Color32::TRANSPARENT.to_normalized_gamma_f32()
    }
}

/// 关掉窗口显示/隐藏时的 DWM 过渡动画。
/// Windows 默认会给隐藏的窗口做一段淡出,BitBlt 会拍到淡出中的面板残影;
/// 关掉后 Hide 立即生效,进入标注前抓的底图里才没有待机面板。
fn disable_transitions(hwnd: HWND) {
    let on = windows::core::BOOL(1);
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_TRANSITIONS_FORCEDISABLED,
            &on as *const _ as *const std::ffi::c_void,
            std::mem::size_of::<windows::core::BOOL>() as u32,
        );
    }
}

/// egui 默认字体没有 CJK 字形,从系统字体目录加载一个中文字体做回退
fn load_cjk_font(ctx: &Context) {
    let candidates = [
        r"C:\Windows\Fonts\simhei.ttf",
        r"C:\Windows\Fonts\Deng.ttf",
        r"C:\Windows\Fonts\msyh.ttc",
    ];
    for path in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            let mut defs = FontDefinitions::default();
            defs.font_data
                .insert("cjk".into(), std::sync::Arc::new(FontData::from_owned(bytes)));
            if let Some(fam) = defs.families.get_mut(&FontFamily::Proportional) {
                fam.push("cjk".into());
            }
            if let Some(fam) = defs.families.get_mut(&FontFamily::Monospace) {
                fam.push("cjk".into());
            }
            ctx.set_fonts(defs);
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一张"像截图"的测试图:渐变底 + 若干色块 + 一条深色细线(文字近似)
    fn test_shot(w: i32, h: i32) -> capture::Shot {
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                rgba[i] = (40 + (x * 120 / w)) as u8;
                rgba[i + 1] = (60 + (y * 100 / h)) as u8;
                rgba[i + 2] = (200 - (x * 90 / w)) as u8;
                rgba[i + 3] = 255;
            }
        }
        for (bx, by, bw, bh, c) in [
            (20, 30, 300, 120, [250u8, 250, 250]),
            (400, 200, 260, 90, [240, 240, 240]),
            (60, 400, 600, 40, [30, 30, 30]),
        ] {
            for y in by..(by + bh).min(h) {
                for x in bx..(bx + bw).min(w) {
                    let i = ((y * w + x) * 4) as usize;
                    rgba[i..i + 3].copy_from_slice(&c);
                }
            }
        }
        capture::Shot { width: w, height: h, rgba }
    }

    fn size_of(shot: &capture::Shot, set: ShotSettings) -> usize {
        let (ext, bytes) = encode_shot(shot, set).expect("编码失败");
        assert!(!bytes.is_empty());
        assert!(ext == "webp" || ext == "png");
        bytes.len()
    }

    fn webp(quality: u8, lossless: bool) -> ShotSettings {
        ShotSettings { format: ShotFormat::Webp, quality, lossless }
    }

    #[test]
    fn webp_output_has_riff_container() {
        let shot = test_shot(320, 240);
        let (ext, bytes) = encode_shot(&shot, webp(85, false)).unwrap();
        assert_eq!(ext, "webp");
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WEBP");
    }

    #[test]
    fn lossy_webp_beats_png() {
        let shot = test_shot(800, 600);
        let png = size_of(&shot, ShotSettings { format: ShotFormat::Png, quality: 85, lossless: false });
        let lossy = size_of(&shot, webp(85, false));
        assert!(lossy < png, "有损 WebP({lossy}) 应小于 PNG({png})");
    }

    #[test]
    fn lossless_webp_beats_png() {
        let shot = test_shot(800, 600);
        let png = size_of(&shot, ShotSettings { format: ShotFormat::Png, quality: 85, lossless: false });
        let lossless = size_of(&shot, webp(85, true));
        assert!(lossless < png, "无损 WebP({lossless}) 应小于 PNG({png})");
    }

    #[test]
    fn lower_quality_means_smaller_file() {
        let shot = test_shot(800, 600);
        let low = size_of(&shot, webp(30, false));
        let high = size_of(&shot, webp(95, false));
        assert!(low < high, "清晰度 30({low}) 应小于 95({high})");
    }

    #[test]
    fn human_size_reads_in_kb_and_mb() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(2048), "2.0 KB");
        assert_eq!(human_size(3 * 1024 * 1024), "3.00 MB");
    }
}
