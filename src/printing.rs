//! 打印 RGBA 图到默认打印机:GDI 打印,自动等比适配页面并居中。

use windows::core::{HSTRING, PCWSTR, PWSTR};
use windows::Win32::Graphics::Gdi::{
    CreateDCW, DeleteDC, GetDeviceCaps, StretchDIBits, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    DIB_RGB_COLORS, HORZRES, SRCCOPY, VERTRES,
};
use windows::Win32::Graphics::Printing::GetDefaultPrinterW;
use windows::Win32::Storage::Xps::{EndDoc, EndPage, StartDocW, StartPage, DOCINFOW};

use crate::i18n::Lang;

/// 打印一张 RGBA 图(内部转 BGRA,等比适配页面居中)
pub fn print_rgba(rgba: &[u8], w: i32, h: i32, lang: Lang) -> Result<(), String> {
    let tr = lang.strings();
    if w <= 0 || h <= 0 || rgba.len() < (w as usize * h as usize * 4) {
        return Err(tr.err_invalid_image.into());
    }
    // RGBA → BGRA(StretchDIBits 需要 BI_RGB 32bpp BGRA、top-down)
    let mut bgra = Vec::with_capacity(rgba.len());
    for px in rgba.chunks_exact(4) {
        bgra.push(px[2]);
        bgra.push(px[1]);
        bgra.push(px[0]);
        bgra.push(px[3]);
    }
    unsafe {
        // 默认打印机名(第一次调用拿所需长度)
        let mut len: u32 = 0;
        let _ = GetDefaultPrinterW(None, &mut len);
        if len == 0 {
            return Err(tr.err_no_printer.into());
        }
        let mut buf = vec![0u16; len as usize];
        if !GetDefaultPrinterW(Some(PWSTR(buf.as_mut_ptr())), &mut len).as_bool() {
            return Err(tr.err_get_printer.into());
        }
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        let printer = HSTRING::from_wide(&buf[..end]);

        let hdc = CreateDCW(PCWSTR::null(), &printer, PCWSTR::null(), None);
        if hdc.is_invalid() {
            return Err(tr.err_create_dc.into());
        }

        let page_w = GetDeviceCaps(Some(hdc), HORZRES);
        let page_h = GetDeviceCaps(Some(hdc), VERTRES);
        if page_w <= 0 || page_h <= 0 {
            let _ = DeleteDC(hdc);
            return Err(tr.err_page_size.into());
        }
        let scale = (page_w as f64 / w as f64).min(page_h as f64 / h as f64);
        let dw = (w as f64 * scale).round() as i32;
        let dh = (h as f64 * scale).round() as i32;
        let dx = (page_w - dw) / 2;
        let dy = (page_h - dh) / 2;

        let doc_name = HSTRING::from(tr.print_doc_name);
        let di = DOCINFOW {
            cbSize: std::mem::size_of::<DOCINFOW>() as i32,
            lpszDocName: PCWSTR(doc_name.as_ptr()),
            lpszOutput: PCWSTR::null(),
            lpszDatatype: PCWSTR::null(),
            fwType: 0,
        };
        if StartDocW(hdc, &di) <= 0 || StartPage(hdc) <= 0 {
            let _ = DeleteDC(hdc);
            return Err(tr.err_start_print.into());
        }

        let mut bmi = BITMAPINFO::default();
        bmi.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            biSizeImage: (w as u32) * (h as u32) * 4,
            ..Default::default()
        };
        StretchDIBits(
            hdc,
            dx, dy, dw, dh,
            0, 0, w, h,
            Some(bgra.as_ptr().cast()),
            &bmi,
            DIB_RGB_COLORS,
            SRCCOPY,
        );

        let _ = EndPage(hdc);
        let _ = EndDoc(hdc);
        let _ = DeleteDC(hdc);
    }
    Ok(())
}
