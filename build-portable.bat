@echo off
chcp 65001 >nul
setlocal EnableExtensions
title PointToFix 便携版打包

rem 纯 Rust 程序:直接输出「exe + onnxruntime.dll + ISNet 模型」的便携目录,不再用 Enigma 加壳。
rem 目标目录可以直接拷到任何 Windows 10/11 x64 机器上运行;没有 DLL/模型时仅键控去背可用。

cd /d "%~dp0"
set "ROOT=%~dp0"
set "INPUT=%ROOT%target\release\pointtofix.exe"
set "OUTDIR=%ROOT%dist"
set "PKGDIR=%OUTDIR%\pointtofix-portable"
set "ZIP=%OUTDIR%\pointtofix-portable.zip"
set "MODELDIR=%ROOT%models"

rem 双击运行时 PATH 里可能没有 cargo
where cargo >nul 2>&1
if errorlevel 1 if exist "%USERPROFILE%\.cargo\bin\cargo.exe" set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"

echo ========================================
echo  1/3 编译 release 版
echo ========================================
cargo build --release
if errorlevel 1 (
    echo [错误] cargo build --release 失败
    goto :fail
)
if not exist "%INPUT%" (
    echo [错误] 未找到编译产物: %INPUT%
    goto :fail
)

echo.
echo ========================================
echo  2/3 组装便携目录
echo ========================================
set "ORT_DLL="
set "ISNET_MODEL="
if exist "%MODELDIR%\onnxruntime.dll" set "ORT_DLL=%MODELDIR%\onnxruntime.dll"
if not defined ORT_DLL if exist "%ROOT%onnxruntime.dll" set "ORT_DLL=%ROOT%onnxruntime.dll"

rem 按优先级挑一个模型: 用户命名 -^> fp16 -^> fp32 -^> int8 -^> 任意 isnet*.onnx
for %%N in (isnet-general-uxe.onnx isnet-general-use-fp16.onnx isnet-general-use.onnx isnet-general-use-int8.onnx isnet-general-use-q8.onnx) do (
    if not defined ISNET_MODEL if exist "%MODELDIR%\%%N" set "ISNET_MODEL=%MODELDIR%\%%N"
)
if not defined ISNET_MODEL for %%F in ("%MODELDIR%\isnet*.onnx") do (
    if not defined ISNET_MODEL set "ISNET_MODEL=%%~fF"
)

if not exist "%OUTDIR%" mkdir "%OUTDIR%"
if exist "%PKGDIR%" rmdir /s /q "%PKGDIR%"
mkdir "%PKGDIR%"
copy /y "%INPUT%" "%PKGDIR%\pointtofix.exe" >nul
if errorlevel 1 (
    echo [错误] 复制 pointtofix.exe 失败
    goto :fail
)

if defined ORT_DLL (
    copy /y "%ORT_DLL%" "%PKGDIR%\onnxruntime.dll" >nul
    echo   推理库: onnxruntime.dll
) else (
    echo   [提示] 没有 models\onnxruntime.dll,便携版只有键控去背
)

if defined ISNET_MODEL (
    for %%F in ("%ISNET_MODEL%") do copy /y "%%~fF" "%PKGDIR%\%%~nxF" >nul
    echo   ISNet 模型: %ISNET_MODEL%
) else (
    echo   [提示] 没有 ISNet 模型,便携版只有键控去背
)
echo   需要 AI 去背时先运行: python scripts\fetch_isnet.py

rem 附一份简短说明,方便拷给别人(用 PowerShell 写,避免 bat 括号转义问题)
powershell -NoProfile -Command ^
  "$lines = @('PointToFix 便携版','','直接双击 pointtofix.exe 运行,不需要安装。','','文件说明:','  pointtofix.exe          主程序','  onnxruntime.dll         ONNX Runtime 推理库,ISNet 去背用','  isnet*.onnx             ISNet 去背模型','','删掉 dll / onnx 后程序仍可使用,但选区去背只剩键控模式。','设置 Ctrl+I 里可以切换 自动 / 键控 / ISNet。','');" ^
  "[IO.File]::WriteAllLines('%PKGDIR%\使用说明.txt', $lines, (New-Object Text.UTF8Encoding $true))"

echo.
echo ========================================
echo  3/3 压缩成 zip(可选)
echo ========================================
if exist "%ZIP%" del /f /q "%ZIP%"
powershell -NoProfile -Command "Compress-Archive -Path '%PKGDIR%\*' -DestinationPath '%ZIP%' -Force" >nul 2>&1
if exist "%ZIP%" (
    echo   已生成: %ZIP%
) else (
    echo   [提示] 压缩失败或系统没有 Compress-Archive,直接用目录即可: %PKGDIR%
)

echo.
echo ========================================
echo   打包成功
echo ========================================
echo   目录: %PKGDIR%
for %%F in ("%PKGDIR%\pointtofix.exe") do echo   exe 大小: %%~zF 字节
if exist "%ZIP%" for %%F in ("%ZIP%") do echo   zip 大小: %%~zF 字节
echo.
if /i not "%~1"=="nopause" pause
exit /b 0

:fail
echo.
if /i not "%~1"=="nopause" pause
exit /b 1
