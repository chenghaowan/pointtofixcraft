<div align="center">

<img src="assets/pointtofixcraft.png" width="112" alt="PointToFix Craft icon">

# PointToFix Craft

**A tiny, single-file screen annotation tool for Windows — written in Rust.**

Freeze the screen, draw on it, wipe the background behind a selection, then save, copy, or print.

[![Platform](https://img.shields.io/badge/platform-Windows%2010%2F11%20x64-0078D6?logo=windows&logoColor=white)](#requirements)
[![Rust](https://img.shields.io/badge/rust-2021-DEA584?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Version](https://img.shields.io/badge/version-0.2.0-2ea44f)](https://github.com/chenghaowan/pointtofixcraft/releases)
[![License](https://img.shields.io/badge/license-MIT-blue)](LICENSE)

[Download](#download) · [Features](#features) · [Hotkeys](#hotkeys) · [Build](#build-from-source) · [Architecture](#architecture) · [Roadmap](#roadmap) · [中文说明](README.zh-CN.md)

</div>

---

PointToFix Craft is a clean-room Rust reimplementation of the classic Windows annotation tool [Pointofix](https://www.pointofix.de/). The UI and workflow deliberately mirror Pointofix — a small standby panel, a vertical icon toolbar, freeze-and-annotate on a separate layer — but every line of code and every icon here was written from scratch. **No code, resources, or assets were extracted from the original.**

It is built for the everyday "explain something on screen" job: annotate while presenting, mark up a screenshot, clean up a product photo, or print a marked-up view — without installing anything.

## Download

Grab the portable build from the [**Releases**](https://github.com/chenghaowan/pointtofixcraft/releases) page:

| File | Contents | Size |
| --- | --- | --- |
| `pointtofix-craft-portable.zip` | `pointtofix.exe` + `onnxruntime.dll` | ~13 MB (36 MB unpacked) |

Unzip anywhere and double-click `pointtofix.exe`. No installer, no VC++ redistributable, no registry writes — the tool is a single statically linked executable. Settings live in `%APPDATA%\pointtofix\`.

> **AI background removal is optional.** The download ships without the ISNet model. Keying-based background removal works out of the box; to enable the AI engine, drop an ISNet model into `models\` (or the program folder) — see [Background removal](#background-removal).

## Features

**Standby & session**
- A small always-on-top panel in the corner of the screen. Drag it by the top bar, click **Start** to annotate, click **×** to quit.
- Panel position is remembered in **physical pixels**, so it stays put across monitors with different DPI scaling.
- **Multi-monitor, one screen at a time:** the overlay covers exactly the display the Start button was on. Other displays are untouched — nothing is captured, drawn, or blocked there. Save/copy/print also only contain that display, rendered at its own DPI.
- The overlay follows resolution and scaling changes, and falls back to the primary display when a monitor is unplugged.

**Drawing**
- Vertical toolbar with hand-drawn vector icons and localized tooltips, matching the original column layout.
- Two color columns: **opaque** on the left, **transparent (highlighter)** on the right — applies to every tool.
- Pen widths **2 / 4 / 8 / 12 px**, which also scale the check/cross stamps and the eraser.
- Pen, **circular eraser**, line, arrow, rectangle, filled rectangle, ellipse, filled ellipse, double arrow, text, ✓ / ✗ stamps.
- **The eraser is a brush, not "delete whole stroke":** it cuts strokes, arrows, rectangles and ellipses inside the circle, punches a circular hole out of filled shapes, and removes only the arrow head when you circle it.
- `Shift` snaps lines and arrows to 45° increments, rectangles to squares, ellipses to circles.
- Undo / redo stack, clear-all.

**Selection & export**
- **Region select** (`Ctrl+R`): drag a marquee, keep it visible with its pixel dimensions; save/copy/print apply to the selection, otherwise to the whole screen. Ink is clipped to the marquee so the export matches what you drew. The selection is persisted in `%APPDATA%\pointtofix\region.txt` and restored on the next session.
- **Zoom the selection** ×1 → ×2 → ×4 → ×8 → ×10, anchored at the selection center on the frozen frame.
- **Save** writes `pointtofix_<timestamp>.webp` to the desktop (switchable to PNG), **copy** puts the annotated screen on the clipboard as uncompressed RGBA, **print** goes to the default printer, scaled to fit and centered.
- WebP quality is adjustable **1–100**, or choose **lossless** for a bit-identical result. Measured on a 2560×1600 desktop: **PNG 2.50 MB → WebP q85 255 KB (~1/10)**, lossless WebP 1.22 MB.
- The toolbar and marquee are hidden in exports by default; **debug mode** includes them so you can report UI bugs.

**Extras**
- **Magnifier** (`Ctrl+M`): follows the cursor, 1.5×–8× via the mouse wheel.
- **Selected-region background removal** (`Ctrl+D` or the ⊠ button), see below.
- **i18n**: 中文 / English / Deutsch / 日本語, following the system UI language on first launch.
- **Layered cancel**: `Esc` cancels an active marquee → clears a selection → closes the text editor → exits the magnifier → leaves annotation mode.

## Hotkeys

| Key | Action |
| --- | --- |
| `Esc` | Layered cancel / leave annotation mode |
| `Ctrl+R` | Select a region (select only, no capture) |
| `Ctrl+S` | Save (selection if present, otherwise the whole screen) |
| `Ctrl+C` | Copy to clipboard (same rule) |
| `Ctrl+D` | Toggle background removal inside the selection |
| `Ctrl+Z` / `Ctrl+Y` | Undo / redo |
| `Ctrl+M` | Toggle the cursor magnifier |
| `Ctrl+I` | Settings (language, save format, background engine, debug mode) |
| Wheel | Magnifier zoom factor |

> Hotkeys use `Ctrl` combinations on purpose: with a Chinese IME active, bare letter keys get swallowed by composition.

## Background removal

The ⊠ button (or `Ctrl+D`) whitens the background **inside the selection** — useful for turning a screenshot or a photo into something you can paste into a document. Ink lives on a separate vector layer and is never part of the sampled image, so arrows, stamps and text are never erased; the white result is composited *under* the ink so you can keep annotating on it. Press again to restore.

Two engines, selectable in Settings:

| Engine | What it does |
| --- | --- |
| **Keying** (default fallback) | Treats the background as the region **connected to the marquee border** — not a global color match. Side faces and bore walls that are a few shades off from the background stay. Closed background islands that are not border-connected are removed only when their mean color distance is ≤ 3 (table cells, hole bottoms). Highlights on walls and part faces are kept. Skips the outermost 2 px so a screenshot border or marquee outline cannot poison the key. |
| **ISNet ONNX** | A learned foreground mask for photos with complex backgrounds. Auto mode runs keying first and only falls back to ISNet when keying cannot find a stable background — so thin lines, grids and text are never wiped out by the model. |

Model files are searched in the program folder, `models\`, and `%APPDATA%\pointtofix\models`, and can be overridden with `POINTTOFIX_ISNET_MODEL` / `POINTTOFIX_ORT_DLL`. Inference runs through **ONNX Runtime** (`onnxruntime.dll`, loaded dynamically at runtime — nothing is downloaded during build) on a background thread, with a progress indicator over the selection.

| Model | Size | Measured (1024×1024, this machine) | Notes |
| --- | --- | --- | --- |
| `isnet-general-use-fp16.onnx` | **88 MB** | **≈2.5 s** | **Recommended** — fp32 quality at half the size, slightly faster |
| `isnet-general-use.onnx` | 176 MB | ≈2.6 s | Official rembg fp32 model (Apache-2.0) |
| `isnet-general-use-int8.onnx` | **44 MB** | ≈2.8 s | int8 quantized, smallest; slightly slower on CPU |

Prepare the model and runtime in one command:

```bash
python scripts/fetch_isnet.py                # fp16 (88 MB, recommended)
python scripts/fetch_isnet.py --variant int8 # int8 (44 MB, smallest)
python scripts/fetch_isnet.py --variant fp32 # fp32 (176 MB, official)
```

Sources: [onnx-community/ISNet-ONNX](https://www.modelscope.cn/models/onnx-community/ISNet-ONNX) and [dragonsoar/imging-background-removal](https://www.modelscope.cn/models/dragonsoar/imging-background-removal) on ModelScope (mirrors that are directly reachable from mainland China), or the official rembg `isnet-general-use.onnx`.

## Settings & persistence

`Ctrl+I` opens the settings window (language, screenshot format and quality, background-removal engine and model status, about, debug mode). Changes apply immediately.

Everything is stored as plain text under `%APPDATA%\pointtofix\`:

| File | Contents |
| --- | --- |
| `ui.txt` | `lang=zh\|en\|de\|ja`, `format=webp\|png`, `quality=1..100`, `lossless=1` |
| `region.txt` | Selection rectangle, restored on the next session |
| `models\` | Optional drop-in location for ISNet models and `onnxruntime.dll` |

## Build from source

### Requirements

- Windows 10/11 (x64)
- [Rust](https://rustup.rs/) stable with the **MSVC** toolchain (Visual Studio Build Tools with the C++ workload)
- ~5 minutes for the first release build (libwebp's C sources are compiled as part of the `webp` crate)

### Build

```bash
git clone https://github.com/chenghaowan/pointtofixcraft.git
cd pointtofixcraft

cargo run              # debug build, keeps the console for POINTTOFIX_* debug output
cargo build --release  # target/release/pointtofix.exe, ~17 MB
```

The release binary is self-contained: keying background removal, annotation, WebP/PNG encoding and printing all work with just the exe. The release profile enables `strip` and thin LTO; `windows_subsystem = "windows"` keeps the console hidden. Only AI background removal needs `onnxruntime.dll` and a model beside the binary.

### Portable package

```bat
build-portable.bat            :: compiles release, assembles dist\pointtofix-portable\ and zips it
build-portable.bat nopause    :: same, for scripts / CI
```

The script picks up `models\onnxruntime.dll` and any `models\isnet*.onnx`; when they are absent the package is exe-only and keying removal still works.

### Debug environment variables

| Variable | Effect |
| --- | --- |
| `POINTTOFIX_START_ACTIVE=1` | Skip the standby panel and enter annotation mode at launch |
| `POINTTOFIX_CLICKTEST=1` | Trigger a background-removal pass 10 frames after entering annotation mode (for automation) |
| `POINTTOFIX_GEOM_LOG=1` | Print viewport / window / toolbar geometry to stderr |
| `POINTTOFIX_BAR_SCALE=0.8` | Force the toolbar scale, to test the "screen too short" layout |

## Architecture

PointToFix Craft keeps the original structure — *two windows and a display list*:

| Concept | Implementation |
| --- | --- |
| Full-screen transparent drawing layer | Borderless always-on-top `eframe` window sized to the **current display's physical rect**, with the frozen desktop capture as an opaque backdrop and ink drawn on `Order::Background` |
| Vertical toolbar | An `egui` floating window inside the same viewport, above the canvas |
| VCL/GDI drawing | Vector ink list (`Vec<Ink>`) repainted by `epaint`; undo = pop the stack |
| Print / save screen contents | `BitBlt(SRCCOPY \| CAPTUREBLT)` captures layered windows too, so ink is included |

> **Why the frozen backdrop instead of a transparent window:** on numerous AMD/NVIDIA drivers and Remote Desktop sessions, a wgpu transparent surface reports `Opaque` and composites to black — that is what made "Start annotation" go black on some machines. Capturing the display up front and painting it as an opaque base matches the original's behavior (the screen is frozen anyway) and removes the dependency on window transparency entirely.

```
src/
├── main.rs     # entry point: window attributes (transparent / borderless / topmost) + window icon
├── app.rs      # state machine (Standby ⇄ Active), frozen backdrop, toolbar & settings UI,
│               # input handling, hotkeys, export encoding (WebP/PNG)
├── i18n.rs     # UI strings for 中文 / English / Deutsch / 日本語 (static tables)
├── icons.rs    # vector toolbar icons drawn with epaint (no icon font dependency)
├── ink.rs      # ink model (vector shapes + repaint + undo/redo stack)
├── capture.rs  # Win32 GDI capture by display/region (BGRA→RGBA, monitor enumeration)
├── bgremove.rs # background removal: keying algorithm + background job queue (keying/ISNet auto fallback)
├── isnet.rs    # ISNet model discovery, ONNX Runtime inference, mask post-processing (matting off)
└── printing.rs # GDI printing (default printer, fit-to-page, centered)

assets/                # app.ico (multi-size PE resource) + pointtofixcraft.png (window icon)
build.rs               # embeds assets/app.ico into the PE resource section via embed-resource
scripts/make_icon.py   # regenerates the icon (Python + Pillow)
scripts/fetch_isnet.py # downloads the ISNet model + prepares onnxruntime.dll
build-portable.bat     # release build + portable dist folder + zip
```

The icon keeps the Pointofix idea of a four-color disc with a red marker and a pointer, with a Rust gear badge in the corner; run `python scripts/make_icon.py` after editing it.

## Roadmap

Already planned, in rough order:

- [ ] Exports without the toolbar (compose backdrop + ink vectors instead of a full-screen `BitBlt`)
- [ ] Click-through while idle, and temporary pass-through to the window underneath
- [ ] Global hotkey to toggle annotation mode, plus a tray icon
- [ ] Original-style full-screen zoom (up to 10×) with panning and a color picker
- [ ] Editable text boxes (move / re-edit before committing, `Ctrl+Enter` for newlines)
- [ ] Draggable selection with exact position/size entry (`F2`)
- [ ] JPG/BMP export, and "send by mail"
- [ ] Whiteboard / blackboard page backgrounds
- [ ] More settings (cursor visibility, wheel-zoom toggle, startup/exit behavior, auto-capture, button size, custom colors)

## Reverse-engineering notes

The reference build is Pointofix 1.8.0: a 3 MB Delphi/C++Builder VCL executable, linked only against system DLLs, no network, no packer. Its observable design is essentially the architecture above — a full-screen `TScreenForm` drawing layer, a vertical `TToolForm` toolbar, and a graphics list. This project reproduces the *behavior* for interoperability and learning; it contains no extracted code, resources, or binaries.

Pointofix is the work of its author at [pointofix.de](https://www.pointofix.de/); this project is not affiliated with or endorsed by them.

## License

[MIT](LICENSE) © chenghao.wan

The optional ISNet models are **not** part of this repository or its source license; the official `isnet-general-use.onnx` from rembg is distributed under Apache-2.0.
