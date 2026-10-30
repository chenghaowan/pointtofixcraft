"""一键准备 ISNet 去背所需的两个文件(都放到 models/ 目录):

    models\\onnxruntime.dll              推理库(ONNX Runtime 官方 CPU 版)
    models\\isnet-general-use-fp16.onnx   ISNet 模型(默认 fp16,88 MB)

模型从魔搭社区(ModelScope)下载,国内直连可用:
    https://www.modelscope.cn/models/onnx-community/ISNet-ONNX
可选的 --variant:
    fp16  isnet-general-use-fp16.onnx  88 MB  —— 推荐:画质≈fp32,速度快一倍
    fp32  isnet-general-use.onnx      176 MB —— 官方原版
    int8  isnet-general-int8.onnx      44 MB —— 最小,CPU 上略慢

onnxruntime.dll 的来源(按顺序尝试):
    1. --ort-dll 参数指定的路径
    2. 已安装的 python 包 onnxruntime(\"import onnxruntime\" 能拿到 capi 目录)
    3. 用 pip 临时装一份 onnxruntime,只取它的 DLL 出来

用法:
    python scripts/fetch_isnet.py                 # 默认 fp16
    python scripts/fetch_isnet.py --variant int8
    python scripts/fetch_isnet.py --ort-dll D:\\somewhere\\onnxruntime.dll
"""

import argparse
import os
import shutil
import subprocess
import sys
import tempfile
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_TARGET = ROOT / "models"

MODELSCOPE = "https://www.modelscope.cn/models"

VARIANTS = {
    # 变体名: (仓库, 仓库内路径, 落地文件名)
    "fp16": ("onnx-community/ISNet-ONNX", "onnx/model_fp16.onnx", "isnet-general-use-fp16.onnx"),
    "fp32": ("onnx-community/ISNet-ONNX", "onnx/model.onnx", "isnet-general-use.onnx"),
    "int8": (
        "dragonsoar/imging-background-removal",
        "isnet-general-int8.onnx",
        "isnet-general-use-int8.onnx",
    ),
}


def log(msg: str) -> None:
    print(msg, flush=True)


def download_model(variant: str, target_dir: Path) -> Path:
    repo, remote_path, out_name = VARIANTS[variant]
    out = target_dir / out_name
    if out.is_file() and out.stat().st_size > 1_000_000:
        log(f"[跳过] 模型已存在: {out}")
        return out

    url = f"{MODELSCOPE}/{repo}/resolve/master/{remote_path}"
    log(f"[下载] {url}")
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
                pct = done * 100 // total
                print(f"\r  {done/1e6:6.1f} / {total/1e6:6.1f} MB ({pct}%)", end="", flush=True)
    print()
    tmp.replace(out)
    log(f"[完成] {out} ({out.stat().st_size/1e6:.1f} MB)")
    return out


def dll_from_installed_package() -> Path | None:
    try:
        import onnxruntime  # noqa: F401
    except ImportError:
        return None
    capi = Path(onnxruntime.__file__).parent / "capi"
    dll = capi / "onnxruntime.dll"
    return dll if dll.is_file() else None


def dll_from_pip_install() -> Path | None:
    """临时目录里 pip 装一份,只取 DLL。用当前 pip 配置的镜像。"""
    try:
        tmp = Path(tempfile.mkdtemp(prefix="ptf-ort-"))
        log("[pip] 临时安装 onnxruntime(只为了取 DLL)…")
        subprocess.run(
            [sys.executable, "-m", "pip", "install", "--quiet", "--no-input",
             "--disable-pip-version-check", "--target", str(tmp), "onnxruntime"],
            check=True,
        )
        dll = tmp / "onnxruntime" / "capi" / "onnxruntime.dll"
        if dll.is_file():
            return dll
        return None
    except Exception as e:  # noqa: BLE001
        log(f"[警告] pip 安装 onnxruntime 失败: {e}")
        return None


def prepare_dll(explicit: str | None, target_dir: Path) -> Path | None:
    out = target_dir / "onnxruntime.dll"
    if explicit:
        src = Path(explicit)
        if not src.is_file():
            log(f"[错误] --ort-dll 指向的文件不存在: {src}")
            return None
    else:
        src = dll_from_installed_package() or dll_from_pip_install()
        if src is None:
            log("[错误] 没找到 onnxruntime.dll;请用 --ort-dll 指定,或先 pip install onnxruntime")
            return None

    if out.is_file() and out.stat().st_size == src.stat().st_size:
        log(f"[跳过] DLL 已存在: {out}")
        return out
    shutil.copy2(src, out)
    log(f"[完成] {out} ({out.stat().st_size/1e6:.1f} MB) <- {src}")

    # providers 插件 DLL 在 CPU 版里用不到,但同目录一起拷更保险(ORT 找不到时会忽略)
    for extra in ("onnxruntime_providers_shared.dll",):
        s = src.parent / extra
        if s.is_file():
            shutil.copy2(s, target_dir / extra)
    return out


def main() -> int:
    ap = argparse.ArgumentParser(description="准备 ISNet 去背依赖(模型 + onnxruntime.dll)")
    ap.add_argument("--variant", choices=sorted(VARIANTS), default="fp16")
    ap.add_argument("--target", default=str(DEFAULT_TARGET), help="输出目录,默认 models/")
    ap.add_argument("--ort-dll", default=os.environ.get("POINTTOFIX_ORT_DLL"),
                    help="onnxruntime.dll 的来源路径")
    ap.add_argument("--skip-dll", action="store_true", help="只下载模型,不处理 DLL")
    args = ap.parse_args()

    target_dir = Path(args.target)
    target_dir.mkdir(parents=True, exist_ok=True)

    download_model(args.variant, target_dir)
    if not args.skip_dll:
        prepare_dll(args.ort_dll, target_dir)

    log("")
    log(f"全部完成。把 {target_dir} 里的文件放到程序目录或 models 子目录即可启用 ISNet 去背。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
