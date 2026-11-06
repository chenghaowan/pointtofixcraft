//! PointToFix —— 屏幕标注工具(Rust 重写版,REA 练手项目)
//!
//! 启动后是一个小工具条窗口;点"开始标注"进入全屏透明绘图层,
//! Esc 或"完成"退回工具条。
//!
//! 无头模式:带 `--capture x,y,w,h` 参数时不启动 GUI,
//! 直接截取指定区域(可选 OCR、入库画廊),供脚本 / AI 代理调用。

// release 版是窗口程序:双击不弹控制台。debug 构建保留控制台,
// 方便看 POINTTOFIX_GEOM_LOG 等调试钩子的输出。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod airuntime;
mod app;
mod bgremove;
mod capture;
mod cli;
mod download;
mod gallery;
mod i18n;
mod icons;
mod ink;
mod isnet;
mod ocr;
mod tray;
mod zipmin;

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
    // 无头 CLI:pointtofix --capture x,y,w,h [--out path] [--ocr] [--json]
    // 命中 --capture 时不初始化 eframe,截完即退(退出码 0 成功 / 非 0 失败)
    match cli::parse(std::env::args().skip(1)) {
        cli::CliAction::Run(args) => std::process::exit(cli::run(args)),
        cli::CliAction::Bad(msg) => {
            eprintln!("error: {msg}");
            std::process::exit(2);
        }
        cli::CliAction::Gui => {}
    }

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
