"""一键准备 OCR(PP-OCRv5 Mobile)所需的三个文件(都放到 models/ 目录):

    models\\ppocrv5_mobile_det.onnx   文本检测(约 5 MB)
    models\\ppocrv5_mobile_rec.onnx   文本识别(约 16 MB,覆盖简中/繁中/英/日)
    models\\ppocrv5_dict.txt          识别词典(约 0.2 MB)

模型从魔搭社区(ModelScope)的 RapidOCR 仓库下载,国内直连可用:
    https://www.modelscope.cn/models/RapidAI/RapidOCR
词典走 jsdelivr(PaddleOCR 官方仓库镜像),国内外都可达。

应用内设置页也有同样的下载入口(带进度),本脚本是离线手动渠道。
用法:
    python scripts/fetch_ppocr.py                 # 默认下到 <仓库>/models
    python scripts/fetch_ppocr.py --target D:\\somewhere\\models
"""

import sys
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_TARGET = ROOT / "models"

# 落地文件名: (URL, 落地文件名) —— 名字必须与 src/ocr.rs 的发现优先级对得上
FILES = [
    (
        "https://www.modelscope.cn/models/RapidAI/RapidOCR/resolve/master/onnx/PP-OCRv5/det/ch_PP-OCRv5_det_mobile.onnx",
        "ppocrv5_mobile_det.onnx",
    ),
    (
        "https://www.modelscope.cn/models/RapidAI/RapidOCR/resolve/master/onnx/PP-OCRv5/rec/ch_PP-OCRv5_rec_mobile.onnx",
        "ppocrv5_mobile_rec.onnx",
    ),
    (
        "https://cdn.jsdelivr.net/gh/PaddlePaddle/PaddleOCR@main/ppocr/utils/dict/ppocrv5_dict.txt",
        "ppocrv5_dict.txt",
    ),
]


def download(url: str, out: Path) -> None:
    if out.is_file() and out.stat().st_size > 1000:
        print(f"[跳过] 已存在: {out}")
        return
    print(f"[下载] {url}")
    req = urllib.request.Request(url, headers={"User-Agent": "Mozilla/5.0"})
    tmp = out.with_suffix(".part")
    with urllib.request.urlopen(req, timeout=600) as r, open(tmp, "wb") as f:
        total = int(r.headers.get("Content-Length") or 0)
        done = 0
        while True:
            chunk = r.read(1 << 20)
            if not chunk:
                break
            f.write(chunk)
            done += len(chunk)
            if total:
                print(f"\r  {done/1e6:6.1f} / {total/1e6:6.1f} MB ({done * 100 // total}%)", end="", flush=True)
    print()
    tmp.replace(out)
    print(f"[完成] {out} ({out.stat().st_size/1e6:.2f} MB)")


def main() -> int:
    target = Path(sys.argv[sys.argv.index("--target") + 1]) if "--target" in sys.argv else DEFAULT_TARGET
    target.mkdir(parents=True, exist_ok=True)
    for url, name in FILES:
        download(url, target / name)
    print()
    print(f"全部完成。把 {target} 里的文件放到程序目录或 models 子目录即可启用 OCR。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
