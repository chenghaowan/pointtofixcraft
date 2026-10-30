//! 界面文案国际化:中文 / English / Deutsch / 日本語。
//!
//! 所有用户可见文案集中在 `Strings` 里,`Lang::strings()` 按当前语言返回静态表。
//! 带 `{}` 的模板用 [`fill`] 顺序填充参数。

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    Zh,
    En,
    De,
    Ja,
}

impl Lang {
    pub const ALL: [Lang; 4] = [Lang::Zh, Lang::En, Lang::De, Lang::Ja];

    /// 语言自身的名称(语言选择器里使用,不随当前语言变化)
    pub fn native_name(self) -> &'static str {
        match self {
            Lang::Zh => "中文",
            Lang::En => "English",
            Lang::De => "Deutsch",
            Lang::Ja => "日本語",
        }
    }

    /// 持久化到 ui.txt 的短代码
    pub fn code(self) -> &'static str {
        match self {
            Lang::Zh => "zh",
            Lang::En => "en",
            Lang::De => "de",
            Lang::Ja => "ja",
        }
    }

    pub fn from_code(code: &str) -> Option<Lang> {
        match code {
            "zh" => Some(Lang::Zh),
            "en" => Some(Lang::En),
            "de" => Some(Lang::De),
            "ja" => Some(Lang::Ja),
            _ => None,
        }
    }

    /// 首次启动跟随系统 UI 语言;不在四种语言内时用英语
    pub fn detect() -> Lang {
        // GetUserDefaultUILanguage 返回 LANGID,低 10 位是主语言 ID
        let primary = unsafe { windows::Win32::Globalization::GetUserDefaultUILanguage() } as u32 & 0x3FF;
        match primary {
            0x04 => Lang::Zh, // LANG_CHINESE
            0x07 => Lang::De, // LANG_GERMAN
            0x11 => Lang::Ja, // LANG_JAPANESE
            _ => Lang::En,
        }
    }

    pub fn strings(self) -> &'static Strings {
        match self {
            Lang::Zh => &ZH,
            Lang::En => &EN,
            Lang::De => &DE,
            Lang::Ja => &JA,
        }
    }
}

/// 依次把模板里的 `{}` 替换成参数(多余的参数忽略)
pub fn fill(template: &str, args: &[&str]) -> String {
    let mut out = String::with_capacity(template.len() + 32);
    let mut rest = template;
    for arg in args {
        match rest.find("{}") {
            Some(i) => {
                out.push_str(&rest[..i]);
                out.push_str(arg);
                rest = &rest[i + 2..];
            }
            None => break,
        }
    }
    out.push_str(rest);
    out
}

pub struct Strings {
    // 待机面板
    pub start_annotate: &'static str,
    pub exit_program_tip: &'static str,

    // 工具条
    pub done: &'static str,
    /// 模板,含一个 `{}`(笔宽像素值)
    pub pen_width_tip: &'static str,
    pub tool_pen: &'static str,
    pub tool_eraser: &'static str,
    pub tool_line: &'static str,
    pub tool_arrow: &'static str,
    pub tool_rect: &'static str,
    pub tool_rect_filled: &'static str,
    pub tool_ellipse: &'static str,
    pub tool_ellipse_filled: &'static str,
    pub tool_double_arrow: &'static str,
    pub tool_text: &'static str,
    pub tool_check_stamp: &'static str,
    pub tool_cross_stamp: &'static str,
    pub tool_time_stamp: &'static str,
    pub tool_signature: &'static str,
    pub tool_region: &'static str,
    pub tool_debak: &'static str,
    pub tool_zoom_in: &'static str,
    pub tool_zoom_out: &'static str,
    pub tool_undo: &'static str,
    pub tool_clear_all: &'static str,
    pub tool_print: &'static str,
    pub tool_save: &'static str,
    pub tool_copy: &'static str,
    pub tool_settings: &'static str,
    pub color_opaque_tip: &'static str,
    pub color_transparent_tip: &'static str,

    // 签名章
    pub signature_heading: &'static str,
    pub signature_label: &'static str,
    pub signature_empty_tip: &'static str,

    // 提示 / 错误
    pub region_selected: &'static str,
    pub select_region_first: &'static str,
    pub select_region_first_zoom: &'static str,
    pub debak_restored: &'static str,
    pub debak_applied: &'static str,
    /// 模板,含一个 `{}`(保存路径)
    pub saved_tip: &'static str,
    pub copied_tip: &'static str,
    pub printed_tip: &'static str,
    /// 模板,含一个 `{}`(错误详情)
    pub failed_tip: &'static str,
    pub region_prefix: &'static str,

    // 设置窗口
    pub settings_title: &'static str,
    pub language_label: &'static str,
    pub shot_heading: &'static str,
    pub format_label: &'static str,
    pub format_webp: &'static str,
    pub format_png: &'static str,
    pub quality_label: &'static str,
    pub quality_tip: &'static str,
    pub lossless_checkbox: &'static str,
    pub bg_heading: &'static str,
    pub bg_method_label: &'static str,
    pub bg_method_auto: &'static str,
    pub bg_method_keying: &'static str,
    pub bg_method_isnet: &'static str,
    /// 模板,含一个 `{}`(模型文件名)
    pub bg_model_found: &'static str,
    pub bg_model_missing: &'static str,
    pub bg_runtime_found: &'static str,
    pub bg_runtime_missing: &'static str,
    pub bg_model_tip: &'static str,
    pub bg_dark_light_checkbox: &'static str,
    pub debak_processing: &'static str,
    /// 模板,含一个 `{}`(失败原因)
    pub debak_ai_fallback: &'static str,
    pub about_heading: &'static str,
    pub app_desc: &'static str,
    pub shortcuts: &'static str,
    pub tips: &'static str,
    pub debug_checkbox: &'static str,
    pub close: &'static str,

    // 截屏 / 打印错误
    pub err_invalid_region: &'static str,
    pub err_getdc: &'static str,
    pub err_bitblt: &'static str,
    pub err_invalid_image: &'static str,
    pub err_no_printer: &'static str,
    pub err_get_printer: &'static str,
    pub err_create_dc: &'static str,
    pub err_page_size: &'static str,
    pub err_start_print: &'static str,
    pub print_doc_name: &'static str,
}

static ZH: Strings = Strings {
    start_annotate: "开始标注",
    exit_program_tip: "退出程序",

    done: "完成",
    pen_width_tip: "笔宽 {}px",
    tool_pen: "画笔",
    tool_eraser: "橡皮擦:圆形笔刷,只擦圈内内容",
    tool_line: "直线",
    tool_arrow: "箭头",
    tool_rect: "框",
    tool_rect_filled: "实框",
    tool_ellipse: "圆",
    tool_ellipse_filled: "实圆",
    tool_double_arrow: "双箭头",
    tool_text: "文字",
    tool_check_stamp: "打勾符号(点画布盖章)",
    tool_cross_stamp: "打叉符号(点画布盖章)",
    tool_time_stamp: "时间章(点画布盖章:日期+整点)",
    tool_signature: "签名章(点画布盖章,文字在设置里改)",
    tool_region: "框选区域 (Ctrl+R):存图/复制/打印只作用于选区",
    tool_debak: "选区去背(键控或 ISNet ONNX,背景变白);再点一次还原 (Ctrl+D)",
    tool_zoom_in: "放大选框区域(+:×2/4/8/10)",
    tool_zoom_out: "缩小选框区域(−)",
    tool_undo: "撤销 (Ctrl+Z)",
    tool_clear_all: "清空全部",
    tool_print: "打印(有选区只打印选区)",
    tool_save: "保存 (Ctrl+S):有选区时只存选区",
    tool_copy: "复制 (Ctrl+C):有选区时只复制选区",
    tool_settings: "设置 (Ctrl+I)",
    color_opaque_tip: "不透明色",
    color_transparent_tip: "透明色(马克笔,所有工具生效)",
    signature_heading: "签名章",
    signature_label: "签名文字",
    signature_empty_tip: "先在设置(Ctrl+I)里填写签名文字",

    region_selected: "已框选区域:「存图/复制/打印」只作用于该区域;+/− 可放大缩小;Esc 取消选区",
    select_region_first: "先框选一个区域(Ctrl+R)",
    select_region_first_zoom: "先框选一个区域(Ctrl+R),+/− 才能放大缩小",
    debak_restored: "已还原选区原图",
    debak_applied: "选区背景已变白;「存图/复制/打印」将使用处理后的画面",
    saved_tip: "已保存:{}",
    copied_tip: "已复制到剪贴板(含墨迹)",
    printed_tip: "已发送到打印机",
    failed_tip: "失败:{}",
    region_prefix: "[选区] ",

    settings_title: "设置",
    language_label: "语言",
    shot_heading: "截图保存",
    format_label: "格式",
    format_webp: "WebP(推荐:同样画质体积更小)",
    format_png: "PNG(兼容性最好)",
    quality_label: "清晰度",
    quality_tip: "清晰度 1–100:越高越清晰、文件越大;勾选「无损」则与原始画面完全一致(体积仍小于 PNG)",
    lossless_checkbox: "无损压缩",
    bg_heading: "选区去背",
    bg_method_label: "方式",
    bg_method_auto: "自动(键控优先,复杂背景用 ISNet)",
    bg_method_keying: "键控(快速,无需模型)",
    bg_method_isnet: "ISNet 模型(更准,较慢)",
    bg_model_found: "模型:{}",
    bg_model_missing: "未找到 ISNet 模型",
    bg_runtime_found: "推理库:onnxruntime.dll 已就绪",
    bg_runtime_missing: "未找到 onnxruntime.dll(AI 去背不可用)",
    bg_model_tip: "推荐 isnet-general-use-fp16.onnx(约 88 MB,画质≈fp32、速度快一倍);也可用 fp32 的 isnet-general-use.onnx 或 int8 量化版(约 44 MB)。和 onnxruntime.dll 一起放到程序目录、models 子目录或 %APPDATA%\\pointtofix\\models。Matting 关闭(仅使用模型遮罩,不做 alpha matting);细线/表格线较多的图建议用键控或自动",
    bg_dark_light_checkbox: "浅色前景转黑(仅键控;白底更易读)",
    debak_processing: "正在去背,请稍候…",
    debak_ai_fallback: "ISNet 不可用,已改用键控去背:{}",
    about_heading: "关于",
    app_desc: "屏幕标注工具 · Rust 重写版(行为参考 Pointofix)",
    shortcuts: "快捷键:Esc 分级取消/退出,Ctrl+R 框选,Ctrl+S 保存,Ctrl+C 复制,Ctrl+D 选区去背,Ctrl+Z 撤销,Ctrl+Y 重做,Ctrl+M 放大镜,Ctrl+I 设置",
    tips: "提示:左列色不透明、右列色透明(马克笔);Shift 约束直线 45°、矩形正方、椭圆正圆;+/− 放大/缩小选框区域;选区会保留到下次启动",
    debug_checkbox: "调试模式:截图包含工具栏",
    close: "关闭",

    err_invalid_region: "无效区域",
    err_getdc: "GetDC 失败",
    err_bitblt: "BitBlt/GetDIBits 失败",
    err_invalid_image: "图像无效",
    err_no_printer: "未找到默认打印机",
    err_get_printer: "获取默认打印机失败",
    err_create_dc: "创建打印机 DC 失败",
    err_page_size: "读取纸张尺寸失败",
    err_start_print: "开始打印任务失败",
    print_doc_name: "PointToFix 屏幕标注",
};

static EN: Strings = Strings {
    start_annotate: "Start",
    exit_program_tip: "Quit application",

    done: "Done",
    pen_width_tip: "Pen width {}px",
    tool_pen: "Pen",
    tool_eraser: "Eraser: round brush, erases only inside the circle",
    tool_line: "Line",
    tool_arrow: "Arrow",
    tool_rect: "Rectangle",
    tool_rect_filled: "Filled rectangle",
    tool_ellipse: "Ellipse",
    tool_ellipse_filled: "Filled ellipse",
    tool_double_arrow: "Double arrow",
    tool_text: "Text",
    tool_check_stamp: "Check mark (click the canvas to stamp)",
    tool_cross_stamp: "Cross mark (click the canvas to stamp)",
    tool_time_stamp: "Time stamp (click canvas: date + hour)",
    tool_signature: "Signature stamp (click canvas; edit text in Settings)",
    tool_region: "Select region (Ctrl+R): save/copy/print only this area",
    tool_debak: "Remove selection background (keying or ISNet ONNX); click again to restore (Ctrl+D)",
    tool_zoom_in: "Zoom into the selection (+: ×2/4/8/10)",
    tool_zoom_out: "Zoom out of the selection (−)",
    tool_undo: "Undo (Ctrl+Z)",
    tool_clear_all: "Clear all",
    tool_print: "Print (selection only if one is set)",
    tool_save: "Save (Ctrl+S): selection only if one is set",
    tool_copy: "Copy (Ctrl+C): selection only if one is set",
    tool_settings: "Settings (Ctrl+I)",
    color_opaque_tip: "Opaque color",
    color_transparent_tip: "Transparent color (marker, applies to all tools)",
    signature_heading: "Signature stamp",
    signature_label: "Signature text",
    signature_empty_tip: "Set your signature text in Settings (Ctrl+I) first",

    region_selected: "Region selected: Save/Copy/Print apply to this area only; +/− zooms; Esc clears the selection",
    select_region_first: "Select a region first (Ctrl+R)",
    select_region_first_zoom: "Select a region first (Ctrl+R), then zoom with +/−",
    debak_restored: "Selection restored to the original image",
    debak_applied: "Selection background is now white; Save/Copy/Print will use the processed image",
    saved_tip: "Saved: {}",
    copied_tip: "Copied to clipboard (with annotations)",
    printed_tip: "Sent to printer",
    failed_tip: "Failed: {}",
    region_prefix: "[Selection] ",

    settings_title: "Settings",
    language_label: "Language",
    shot_heading: "Screenshots",
    format_label: "Format",
    format_webp: "WebP (recommended: same quality, smaller files)",
    format_png: "PNG (best compatibility)",
    quality_label: "Quality",
    quality_tip: "Quality 1–100: higher is sharper and larger. With \"Lossless\" the image stays pixel-identical (still smaller than PNG)",
    lossless_checkbox: "Lossless",
    bg_heading: "Selection background removal",
    bg_method_label: "Method",
    bg_method_auto: "Auto (keying first, ISNet for complex backgrounds)",
    bg_method_keying: "Keying (fast, no model)",
    bg_method_isnet: "ISNet model (better, slower)",
    bg_model_found: "Model: {}",
    bg_model_missing: "ISNet model not found",
    bg_runtime_found: "Runtime: onnxruntime.dll ready",
    bg_runtime_missing: "onnxruntime.dll not found (AI removal unavailable)",
    bg_model_tip: "Recommended: isnet-general-use-fp16.onnx (~88 MB, fp32-level quality at twice the speed); fp32 isnet-general-use.onnx or an int8 build (~44 MB) also work. Put it next to the app, in a models folder, or in %APPDATA%\\pointtofix\\models, together with onnxruntime.dll. Matting is off: only the model mask is used (no alpha matting). For line art/grids use Keying or Auto.",
    bg_dark_light_checkbox: "Darken light foreground (keying only; better on white)",
    debak_processing: "Removing background, please wait…",
    debak_ai_fallback: "ISNet unavailable; used keying instead: {}",
    about_heading: "About",
    app_desc: "Screen annotation tool · Rust rewrite (behavior modeled on Pointofix)",
    shortcuts: "Shortcuts: Esc cancels step by step / exits; Ctrl+R selects a region; Ctrl+S saves; Ctrl+C copies; Ctrl+D whitens the selection background; Ctrl+Z undoes; Ctrl+Y redoes; Ctrl+M magnifier; Ctrl+I settings",
    tips: "Tips: left column is opaque, right column is transparent (marker); Shift constrains lines to 45°, rectangles to squares, ellipses to circles; +/− zooms the selected region; the selection is restored on next launch",
    debug_checkbox: "Debug mode: include the toolbar in screenshots",
    close: "Close",

    err_invalid_region: "Invalid region",
    err_getdc: "GetDC failed",
    err_bitblt: "BitBlt/GetDIBits failed",
    err_invalid_image: "Invalid image",
    err_no_printer: "No default printer found",
    err_get_printer: "Failed to query the default printer",
    err_create_dc: "Failed to create the printer DC",
    err_page_size: "Failed to read the paper size",
    err_start_print: "Failed to start the print job",
    print_doc_name: "PointToFix screen annotation",
};

static DE: Strings = Strings {
    start_annotate: "Starten",
    exit_program_tip: "Programm beenden",

    done: "Fertig",
    pen_width_tip: "Stiftbreite {}px",
    tool_pen: "Stift",
    tool_eraser: "Radierer: runder Pinsel, radiert nur innerhalb des Kreises",
    tool_line: "Linie",
    tool_arrow: "Pfeil",
    tool_rect: "Rechteck",
    tool_rect_filled: "Gefülltes Rechteck",
    tool_ellipse: "Ellipse",
    tool_ellipse_filled: "Gefüllte Ellipse",
    tool_double_arrow: "Doppelpfeil",
    tool_text: "Text",
    tool_check_stamp: "Häkchen (auf die Leinwand klicken zum Stempeln)",
    tool_cross_stamp: "Kreuz (auf die Leinwand klicken zum Stempeln)",
    tool_time_stamp: "Zeitstempel (Klick auf die Leinwand: Datum + volle Stunde)",
    tool_signature: "Signaturstempel (Klick auf die Leinwand; Text in den Einstellungen)",
    tool_region: "Bereich auswählen (Strg+R): Speichern/Kopieren/Drucken nur für diesen Bereich",
    tool_debak: "Hintergrund der Auswahl entfernen (Keying oder ISNet ONNX); erneut klicken zum Zurücksetzen (Strg+D)",
    tool_zoom_in: "Ausgewählten Bereich vergrößern (+: ×2/4/8/10)",
    tool_zoom_out: "Ausgewählten Bereich verkleinern (−)",
    tool_undo: "Rückgängig (Strg+Z)",
    tool_clear_all: "Alles löschen",
    tool_print: "Drucken (nur Auswahl, falls vorhanden)",
    tool_save: "Speichern (Strg+S): nur Auswahl, falls vorhanden",
    tool_copy: "Kopieren (Strg+C): nur Auswahl, falls vorhanden",
    tool_settings: "Einstellungen (Strg+I)",
    color_opaque_tip: "Deckende Farbe",
    color_transparent_tip: "Transparente Farbe (Marker, gilt für alle Werkzeuge)",
    signature_heading: "Signaturstempel",
    signature_label: "Signaturtext",
    signature_empty_tip: "Bitte zuerst in den Einstellungen (Strg+I) einen Signaturtext festlegen",

    region_selected: "Bereich ausgewählt: Speichern/Kopieren/Drucken gilt nur für diesen Bereich; +/− zoomt; Esc hebt die Auswahl auf",
    select_region_first: "Zuerst einen Bereich auswählen (Strg+R)",
    select_region_first_zoom: "Zuerst einen Bereich auswählen (Strg+R), dann mit +/− zoomen",
    debak_restored: "Auswahl auf das Originalbild zurückgesetzt",
    debak_applied: "Der Hintergrund der Auswahl ist jetzt weiß; Speichern/Kopieren/Drucken verwenden das bearbeitete Bild",
    saved_tip: "Gespeichert: {}",
    copied_tip: "In die Zwischenablage kopiert (mit Anmerkungen)",
    printed_tip: "An den Drucker gesendet",
    failed_tip: "Fehlgeschlagen: {}",
    region_prefix: "[Auswahl] ",

    settings_title: "Einstellungen",
    language_label: "Sprache",
    shot_heading: "Screenshots",
    format_label: "Format",
    format_webp: "WebP (empfohlen: gleiche Qualität, kleinere Dateien)",
    format_png: "PNG (beste Kompatibilität)",
    quality_label: "Qualität",
    quality_tip: "Qualität 1–100: höher = schärfer und größer. Mit \"Verlustfrei\" bleibt das Bild pixelgenau (dennoch kleiner als PNG)",
    lossless_checkbox: "Verlustfrei",
    bg_heading: "Auswahl-Hintergrund entfernen",
    bg_method_label: "Methode",
    bg_method_auto: "Auto (Keying zuerst, ISNet für komplexe Hintergründe)",
    bg_method_keying: "Keying (schnell, ohne Modell)",
    bg_method_isnet: "ISNet-Modell (genauer, langsamer)",
    bg_model_found: "Modell: {}",
    bg_model_missing: "ISNet-Modell nicht gefunden",
    bg_runtime_found: "Laufzeit: onnxruntime.dll bereit",
    bg_runtime_missing: "onnxruntime.dll nicht gefunden (KI-Freistellung nicht verfügbar)",
    bg_model_tip: "Empfohlen: isnet-general-use-fp16.onnx (~88 MB, fp32-Qualität bei doppelter Geschwindigkeit); auch fp32-isnet-general-use.onnx oder eine int8-Version (~44 MB) funktionieren. Neben die EXE, in einen models-Ordner oder nach %APPDATA%\\pointtofix\\models legen – zusammen mit onnxruntime.dll. Matting ist aus: nur die Modellmaske, kein Alpha-Matting. Für Linien/Gitter Keying oder Auto verwenden.",
    bg_dark_light_checkbox: "Helle Vordergrundfarben abdunkeln (nur Keying; besser auf Weiß)",
    debak_processing: "Hintergrund wird entfernt…",
    debak_ai_fallback: "ISNet nicht verfügbar; stattdessen Keying verwendet: {}",
    about_heading: "Über",
    app_desc: "Bildschirm-Annotationstool · Rust-Neuimplementierung (Verhalten angelehnt an Pointofix)",
    shortcuts: "Tastenkürzel: Esc bricht schrittweise ab/beendet; Strg+R wählt einen Bereich; Strg+S speichert; Strg+C kopiert; Strg+D färbt den Hintergrund der Auswahl weiß; Strg+Z rückgängig; Strg+Y wiederholen; Strg+M Lupe; Strg+I Einstellungen",
    tips: "Tipp: Linke Spalte deckend, rechte Spalte transparent (Marker); Umschalt erzwingt 45°-Linien, Quadrate und Kreise; +/− zoomt den ausgewählten Bereich; die Auswahl bleibt bis zum nächsten Start erhalten",
    debug_checkbox: "Debugmodus: Symbolleiste im Screenshot anzeigen",
    close: "Schließen",

    err_invalid_region: "Ungültiger Bereich",
    err_getdc: "GetDC fehlgeschlagen",
    err_bitblt: "BitBlt/GetDIBits fehlgeschlagen",
    err_invalid_image: "Ungültiges Bild",
    err_no_printer: "Kein Standarddrucker gefunden",
    err_get_printer: "Standarddrucker konnte nicht abgefragt werden",
    err_create_dc: "Drucker-DC konnte nicht erstellt werden",
    err_page_size: "Papierformat konnte nicht gelesen werden",
    err_start_print: "Druckauftrag konnte nicht gestartet werden",
    print_doc_name: "PointToFix Bildschirmannotation",
};

static JA: Strings = Strings {
    start_annotate: "注釈開始",
    exit_program_tip: "アプリを終了",

    done: "完了",
    pen_width_tip: "ペン幅 {}px",
    tool_pen: "ペン",
    tool_eraser: "消しゴム:円形ブラシ、円内のみ消去",
    tool_line: "直線",
    tool_arrow: "矢印",
    tool_rect: "矩形",
    tool_rect_filled: "塗りつぶし矩形",
    tool_ellipse: "楕円",
    tool_ellipse_filled: "塗りつぶし楕円",
    tool_double_arrow: "両矢印",
    tool_text: "テキスト",
    tool_check_stamp: "チェックマーク(クリックでスタンプ)",
    tool_cross_stamp: "バツマーク(クリックでスタンプ)",
    tool_time_stamp: "タイムスタンプ(クリックで日付+時刻を押印)",
    tool_signature: "サイン印(クリックで押印、文字は設定で編集)",
    tool_region: "範囲選択 (Ctrl+R):保存/コピー/印刷は選択範囲のみ",
    tool_debak: "選択範囲の背景を除去(キーイングまたは ISNet ONNX);もう一度クリックで復元 (Ctrl+D)",
    tool_zoom_in: "選択範囲を拡大(+:×2/4/8/10)",
    tool_zoom_out: "選択範囲を縮小(−)",
    tool_undo: "元に戻す (Ctrl+Z)",
    tool_clear_all: "すべて消去",
    tool_print: "印刷(選択範囲があればその範囲のみ)",
    tool_save: "保存 (Ctrl+S):選択範囲があればその範囲のみ",
    tool_copy: "コピー (Ctrl+C):選択範囲があればその範囲のみ",
    tool_settings: "設定 (Ctrl+I)",
    color_opaque_tip: "不透明色",
    color_transparent_tip: "透明色(マーカー、全ツールに適用)",
    signature_heading: "サイン印",
    signature_label: "サイン文字",
    signature_empty_tip: "先に設定(Ctrl+I)でサイン文字を入力してください",

    region_selected: "範囲を選択しました:「保存/コピー/印刷」はこの範囲のみ対象;+/− で拡大縮小;Esc で選択解除",
    select_region_first: "先に範囲を選択してください (Ctrl+R)",
    select_region_first_zoom: "先に範囲を選択してください (Ctrl+R)。+/− で拡大縮小できます",
    debak_restored: "選択範囲を元の画像に戻しました",
    debak_applied: "選択範囲の背景を白くしました。「保存/コピー/印刷」は処理後の画像を使用します",
    saved_tip: "保存しました:{}",
    copied_tip: "クリップボードにコピーしました(注釈を含む)",
    printed_tip: "プリンターに送信しました",
    failed_tip: "失敗:{}",
    region_prefix: "[選択範囲] ",

    settings_title: "設定",
    language_label: "言語",
    shot_heading: "スクリーンショット保存",
    format_label: "形式",
    format_webp: "WebP(推奨:同画質でファイルが小さい)",
    format_png: "PNG(互換性重視)",
    quality_label: "画質",
    quality_tip: "画質 1〜100:大きいほど鮮明でファイルも大きくなります。「可逆」を選ぶと元の画面と完全に同一(それでも PNG より小さくなります)",
    lossless_checkbox: "可逆圧縮",
    bg_heading: "選択範囲の背景除去",
    bg_method_label: "方式",
    bg_method_auto: "自動(キーイング優先、複雑な背景は ISNet)",
    bg_method_keying: "キーイング(高速・モデル不要)",
    bg_method_isnet: "ISNet モデル(高精度・低速)",
    bg_model_found: "モデル:{}",
    bg_model_missing: "ISNet モデルが見つかりません",
    bg_runtime_found: "推論ライブラリ:onnxruntime.dll 準備完了",
    bg_runtime_missing: "onnxruntime.dll が見つかりません(AI 背景除去は使用不可)",
    bg_model_tip: "推奨は isnet-general-use-fp16.onnx(約 88 MB、fp32 同等画質で 2 倍速)。fp32 の isnet-general-use.onnx や int8 版(約 44 MB)も使えます。onnxruntime.dll と一緒に exe と同じフォルダ、models サブフォルダ、または %APPDATA%\\pointtofix\\models に置いてください。Matting はオフ(モデルのマスクのみ使用、alpha matting なし)。細い線や表の線が多い図はキーイングまたは自動を推奨。",
    bg_dark_light_checkbox: "明るい前景を暗くする(キーイングのみ、白背景で読みやすく)",
    debak_processing: "背景を除去しています…",
    debak_ai_fallback: "ISNet を利用できないため、キーイングで処理しました:{}",
    about_heading: "このアプリについて",
    app_desc: "画面注釈ツール · Rust リライト版(動作は Pointofix を参考)",
    shortcuts: "ショートカット:Esc で段階的にキャンセル/終了、Ctrl+R 範囲選択、Ctrl+S 保存、Ctrl+C コピー、Ctrl+D 選択範囲の背景を白く、Ctrl+Z 元に戻す、Ctrl+Y やり直し、Ctrl+M 虫眼鏡、Ctrl+I 設定",
    tips: "ヒント:左列は不透明色、右列は透明色(マーカー)。Shift で直線は45°、矩形は正方形、楕円は正円に制約。+/− で選択範囲を拡大/縮小。選択範囲は次回起動時に復元されます",
    debug_checkbox: "デバッグモード:スクリーンショットにツールバーを含める",
    close: "閉じる",

    err_invalid_region: "無効な範囲です",
    err_getdc: "GetDC に失敗しました",
    err_bitblt: "BitBlt/GetDIBits に失敗しました",
    err_invalid_image: "無効な画像です",
    err_no_printer: "既定のプリンターが見つかりません",
    err_get_printer: "既定のプリンターの取得に失敗しました",
    err_create_dc: "プリンター DC の作成に失敗しました",
    err_page_size: "用紙サイズの取得に失敗しました",
    err_start_print: "印刷ジョブの開始に失敗しました",
    print_doc_name: "PointToFix 画面注釈",
};
