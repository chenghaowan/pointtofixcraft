//! 系统托盘:点 × 隐藏待机面板后,托盘是唯一的唤回/退出入口。
//!
//! 关键点:tray-icon 的事件(`MenuEvent` / `TrayIconEvent`)来自 **全局回调线程**,
//! 而不是 eframe 的帧循环——窗口隐藏后 eframe 停止重绘,帧循环里的轮询收不到
//! "退出"事件,所以这里让回调线程直接把事件塞进共享队列,并调用线程安全的
//! `egui::Context::request_repaint()` 唤醒帧循环(隐藏状态下也能被唤醒),
//! 帧循环下一帧从队列取事件处理。这样"显示"走帧循环(要操作 UI 状态),
//! "退出"同样走帧循环(发 ViewportCommand::Close),路径统一。

use std::sync::{Arc, Mutex};

use eframe::egui;

pub enum TrayEvent {
    /// 托盘左键 / 菜单「显示主面板」:隐藏时恢复,可见时仅聚焦
    Show,
    /// 菜单「退出」:真退出(隐藏状态下也能收到)
    Exit,
}

struct Shared {
    events: Mutex<Vec<TrayEvent>>,
}

pub struct Tray {
    shared: Arc<Shared>,
    /// 持有图标实例:drop 时托盘图标自动移除
    _icon: tray_icon::TrayIcon,
}

const TRAY_ID: &str = "pointtofix-tray";
const MENU_SHOW: &str = "ptf-show";
const MENU_EXIT: &str = "ptf-exit";

/// 建托盘(必须在主线程调用);失败不致命(仅失去托盘入口),返回 None
pub fn init(ctx: &egui::Context, show_text: &str, exit_text: &str) -> Option<Tray> {
    let rgba = image::load_from_memory(include_bytes!("../assets/pointtofixcraft.png"))
        .ok()?
        .into_rgba8();
    let (w, h) = rgba.dimensions();
    // 托盘图标按小尺寸重绘更清晰;直接给原图,系统自己缩
    let icon = tray_icon::Icon::from_rgba(rgba.into_raw(), w, h).ok()?;

    let menu = tray_icon::menu::Menu::new();
    let item_show = tray_icon::menu::MenuItem::with_id(MENU_SHOW, show_text, true, None);
    let item_exit = tray_icon::menu::MenuItem::with_id(MENU_EXIT, exit_text, true, None);
    let _ = menu.append_items(&[&item_show, &item_exit]);

    let icon_builder = tray_icon::TrayIconBuilder::new()
        .with_id(TRAY_ID)
        .with_icon(icon)
        .with_tooltip("PointToFix")
        .with_menu(Box::new(menu))
        // 左键点击发 TrayIconEvent(用于唤回面板),右键才弹菜单
        .with_menu_on_left_click(false);

    let tray_icon = icon_builder.build().ok()?;

    let shared = Arc::new(Shared {
        events: Mutex::new(Vec::new()),
    });

    // 菜单事件:全局回调线程。request_repaint 唤醒(可能已隐藏的)帧循环
    let menu_shared = shared.clone();
    let menu_ctx = ctx.clone();
    tray_icon::menu::MenuEvent::set_event_handler(Some(move |e: tray_icon::menu::MenuEvent| {
        let ev = match e.id().as_ref() {
            MENU_SHOW => Some(TrayEvent::Show),
            MENU_EXIT => Some(TrayEvent::Exit),
            _ => None,
        };
        if let Some(ev) = ev {
            if let Ok(mut q) = menu_shared.events.lock() {
                q.push(ev);
            }
            menu_ctx.request_repaint();
        }
    }));

    // 托盘本体点击:左键单击 = 显示/聚焦
    let click_shared = shared.clone();
    let click_ctx = ctx.clone();
    tray_icon::TrayIconEvent::set_event_handler(Some(move |e: tray_icon::TrayIconEvent| {
        if let tray_icon::TrayIconEvent::Click {
            button: tray_icon::MouseButton::Left,
            ..
        } = e
        {
            if let Ok(mut q) = click_shared.events.lock() {
                q.push(TrayEvent::Show);
            }
            click_ctx.request_repaint();
        }
    }));

    Some(Tray {
        shared,
        _icon: tray_icon,
    })
}

impl Tray {
    /// 取走积累的托盘事件(帧循环每帧调用一次)
    pub fn poll(&self) -> Vec<TrayEvent> {
        self.shared
            .events
            .lock()
            .map(|mut q| std::mem::take(&mut *q))
            .unwrap_or_default()
    }
}
