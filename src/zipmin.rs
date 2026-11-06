//! 极简 zip 解包:WanAI Runtime 便携包(Release zip)下载后落地用。
//!
//! 只支持「解出来」,不支持打包;条目方法支持 stored(0)与 deflate(8)。
//! 手写 EOCD / central directory / local header 解析,deflate 走 flate2
//! (已在依赖树内),不校验 CRC32——zip 来自我们自己发布的 Release,
//! 下载层已按 Content-Length 校验完整性,此处省掉 30 行查表代码。

use std::io::Read;
use std::path::{Path, PathBuf};

const EOCD_SIG: [u8; 4] = [0x50, 0x4b, 0x05, 0x06]; // PK\x05\x06
const CDH_SIG: [u8; 4] = [0x50, 0x4b, 0x01, 0x02]; // PK\x01\x02
const LFH_SIG: [u8; 4] = [0x50, 0x4b, 0x03, 0x04]; // PK\x03\x04

fn u16(b: &[u8], at: usize) -> usize {
    u16::from_le_bytes([b[at], b[at + 1]]) as usize
}
fn u32(b: &[u8], at: usize) -> usize {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]) as usize
}

/// 解包 zip 到 dest,strip_components 去掉每个条目前 N 级目录
/// (Release zip 顶层是 wanai-runtime-v0.1.0-windows-x64/,传 1 直接落到 dest 根)。
pub fn extract(zip_path: &Path, dest: &Path, strip_components: usize) -> Result<(), String> {
    let raw = std::fs::read(zip_path).map_err(|e| format!("读 zip 失败: {e}"))?;
    // EOCD 定长 22 字节 + 变长注释;从尾部往前扫签名(注释上限一般 64KB)
    let eocd = raw
        .windows(4)
        .rev()
        .take(66_000)
        .position(|w| w == EOCD_SIG)
        .ok_or("zip 结构损坏:找不到 EOCD")?;
    let eocd_at = raw.len() - eocd - 4;
    let e = &raw[eocd_at..];
    if e.len() < 22 {
        return Err("zip 结构损坏:EOCD 截断".into());
    }
    let count = u16(e, 10);
    let cd_off = u32(e, 16);

    let mut cd = cd_off;
    for _ in 0..count {
        if cd + 4 > raw.len() || raw[cd..cd + 4] != CDH_SIG {
            return Err(format!("zip 结构损坏:central directory @ {cd}"));
        }
        let h = &raw[cd..];
        if h.len() < 46 {
            return Err("zip 结构损坏:CDH 截断".into());
        }
        let method = u16(h, 10);
        let csize = u32(h, 20);
        let name_len = u16(h, 28);
        let extra_len = u16(h, 30);
        let comment_len = u16(h, 32);
        let lfh_off = u32(h, 42);
        let name = String::from_utf8_lossy(&h[46..46 + name_len]).into_owned();
        cd += 46 + name_len + extra_len + comment_len;

        if name.ends_with('/') {
            continue; // 目录条目:按需建父目录即可
        }
        let Some(rel) = strip_prefix_components(&name, strip_components) else {
            continue;
        };

        // 数据起点在 local header 里:local 的 name/extra 长度可能与 CDH 不同,必须重读
        if lfh_off + 30 > raw.len() || raw[lfh_off..lfh_off + 4] != LFH_SIG {
            return Err(format!("zip 结构损坏:local header @ {lfh_off}"));
        }
        let l = &raw[lfh_off..];
        let data_at = lfh_off + 30 + u16(l, 26) + u16(l, 28);
        let data_end = data_at + csize;
        if data_end > raw.len() {
            return Err(format!("zip 结构损坏:条目 {name} 数据越界"));
        }
        let payload = &raw[data_at..data_end];

        let out = dest.join(&rel);
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("建目录失败: {e}"))?;
        }
        match method {
            0 => std::fs::write(&out, payload).map_err(|e| format!("写 {} 失败: {e}", rel.display()))?,
            8 => {
                let mut dec = flate2::read::DeflateDecoder::new(payload);
                let mut buf = Vec::with_capacity(csize * 4);
                dec.read_to_end(&mut buf)
                    .map_err(|e| format!("解压 {rel:?} 失败: {e}"))?;
                std::fs::write(&out, &buf).map_err(|e| format!("写 {} 失败: {e}", rel.display()))?;
            }
            m => return Err(format!("条目 {name} 用了不支持的压缩方法 {m}")),
        }
    }
    Ok(())
}

/// 去掉前 N 级路径组件;全被 strip 掉返回 None。
/// 同时防 zip slip:绝对路径、盘符与任何位置的 `..` 组件一律拒绝。
fn strip_prefix_components(name: &str, strip: usize) -> Option<PathBuf> {
    let name = name.replace('\\', "/");
    if name.starts_with('/') || name.contains(':') {
        return None;
    }
    let parts: Vec<&str> = name.split('/').filter(|s| !s.is_empty()).collect();
    if parts.len() <= strip || parts.iter().any(|p| *p == "..") {
        return None;
    }
    let mut out = PathBuf::new();
    for p in &parts[strip..] {
        out.push(p);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::DeflateEncoder;
    use flate2::Compression;

    /// 测试内手工组一个 zip:local header + deflate 数据 + central dir + EOCD
    fn push_u16(v: &mut Vec<u8>, x: u16) {
        v.extend_from_slice(&x.to_le_bytes());
    }
    fn push_u32(v: &mut Vec<u8>, x: u32) {
        v.extend_from_slice(&x.to_le_bytes());
    }

    fn build_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut cd = Vec::new();
        for (name, data) in entries {
            let mut enc = DeflateEncoder::new(Vec::new(), Compression::default());
            std::io::Write::write_all(&mut enc, data).unwrap();
            let comp = enc.finish().unwrap();
            let lfh_off = out.len() as u32;
            out.extend_from_slice(&LFH_SIG);
            push_u16(&mut out, 20); // version needed
            push_u16(&mut out, 0); // flags
            push_u16(&mut out, 8); // method = deflate
            push_u16(&mut out, 0);
            push_u16(&mut out, 0);
            push_u32(&mut out, 0); // crc(不校验,置 0)
            push_u32(&mut out, comp.len() as u32);
            push_u32(&mut out, data.len() as u32);
            push_u16(&mut out, name.len() as u16);
            push_u16(&mut out, 0); // local extra
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(&comp);

            cd.extend_from_slice(&CDH_SIG);
            push_u16(&mut cd, 20);
            push_u16(&mut cd, 20);
            push_u16(&mut cd, 0);
            push_u16(&mut cd, 8);
            push_u16(&mut cd, 0);
            push_u16(&mut cd, 0);
            push_u32(&mut cd, 0);
            push_u32(&mut cd, comp.len() as u32);
            push_u32(&mut cd, data.len() as u32);
            push_u16(&mut cd, name.len() as u16);
            push_u16(&mut cd, 0);
            push_u16(&mut cd, 0);
            push_u16(&mut cd, 0);
            push_u16(&mut cd, 0);
            push_u32(&mut cd, 0);
            push_u32(&mut cd, lfh_off);
            cd.extend_from_slice(name.as_bytes());
        }
        let cd_off = out.len() as u32;
        out.extend_from_slice(&cd);
        out.extend_from_slice(&EOCD_SIG);
        push_u16(&mut out, 0);
        push_u16(&mut out, 0);
        push_u16(&mut out, entries.len() as u16);
        push_u16(&mut out, entries.len() as u16);
        push_u32(&mut out, cd.len() as u32);
        push_u32(&mut out, cd_off);
        push_u16(&mut out, 0); // comment len
        out
    }

    #[test]
    fn extract_strips_and_deflates() {
        let dir = std::env::temp_dir().join(format!("ptf-zipmin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let zip = dir.join("t.zip");
        let big = vec![7u8; 300_000]; // 跨多块压缩,顺带验证流式 inflate
        let bytes = build_zip(&[
            ("root/app.exe", b"MZfake"),
            ("root/models/onnxruntime.dll", &big),
            ("root/empty_dir/", b""),
        ]);
        std::fs::write(&zip, &bytes).unwrap();

        extract(&zip, &dir.join("out"), 1).expect("解包应成功");
        let out = dir.join("out");
        assert_eq!(std::fs::read(out.join("app.exe")).unwrap(), b"MZfake");
        let dll = std::fs::read(out.join("models").join("onnxruntime.dll")).unwrap();
        assert_eq!(dll.len(), 300_000);
        assert!(dll.iter().all(|b| *b == 7));
        // strip=1 后没有 root/ 残留
        assert!(!out.join("root").exists());

        // strip=2:root/ 整层剥掉 → app.exe 被丢弃,models/onnxruntime.dll 升到根
        extract(&zip, &dir.join("out2"), 2).expect("strip=2 应成功");
        assert!(!dir.join("out2").join("app.exe").exists());
        assert!(dir.join("out2").join("onnxruntime.dll").is_file());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn path_traversal_rejected() {
        assert_eq!(strip_prefix_components("../evil.exe", 1), None);
        assert_eq!(strip_prefix_components("/abs/path", 1), None);
        assert_eq!(strip_prefix_components("C:\\win\\evil", 1), None);
        assert_eq!(
            strip_prefix_components("a/b/c.txt", 2),
            Some(PathBuf::from("c.txt"))
        );
        assert_eq!(strip_prefix_components("a/b", 2), None);
    }
}
