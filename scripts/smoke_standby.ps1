# 冒烟:启动 pointtofix 3 秒确认待机面板不崩,然后结束进程
$p = Start-Process -FilePath 'D:\claw.forvia\pointtofixcraft\target\debug\pointtofix.exe' -PassThru
Start-Sleep -Seconds 3
if (-not $p.HasExited) {
    Stop-Process -Id $p.Id -Force
    Write-Output 'smoke: ran 3s without crash, killed'
} else {
    Write-Output ('smoke: exited early, code=' + $p.ExitCode)
}
