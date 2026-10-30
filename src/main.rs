//! PointToFix —— 屏幕标注工具(Rust 重写版,REA 练手项目)
//!
//! 启动后是一个小工具条窗口;点"开始标注"进入全屏透明绘图层,
//! Esc 或"完成"退回工具条。

// release 版是窗口程序:双击不弹控制台。debug 构建保留控制台,
// 方便看 POINTTOFIX_GEOM_LOG 等调试钩子的输出。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod bgremove;
mod capture;
mod i18n;
mod icons;
mod ink;
mod isnet;
mod printing;

use eframe::egui;

/// exe 资源里的 assets/app.ico 是同一张图,这里再设给窗口/任务栏(无边框窗口也要有图标)
fn load_icon() -> egui::IconData {
    let img = image::load_from_memory(include_bytes!("../assets/pointtofixcraft.png"))
        .expect("assets/pointtofixcraft.png 解码失败")
        .into_rgba8();
    let (width, height) = img.dimensions();
    egui::IconData {
        rgba: img.into_raw(),
        width,
        height,
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size(app::STANDBY_SIZE)
            .with_resizable(false)
            .with_decorations(false)
            .with_transparent(true)
            .with_window_level(egui::WindowLevel::AlwaysOnTop)
            .with_icon(std::sync::Arc::new(load_icon())),
        ..Default::default()
    };
    eframe::run_native(
        "PointToFix",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    )
}
