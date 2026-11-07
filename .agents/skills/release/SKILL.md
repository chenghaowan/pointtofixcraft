---
name: release
description: PointToFix Craft 发版流程——bump/打包/打 tag、GitHub+Gitee 双仓库推送与 Release 创建、Gitee API 令牌用法与全部已知坑(WAF 403、PATCH 字段、编码、代理下载停滞)。适用于用户要求"提交/推送 github/gitee"、"发 release"、"发版"、"更新 release 资产"、"bump 版本"等场景。
---

# PointToFix 双仓库发版手册

按顺序执行,每一步的命令都是验证过能直接跑的;**"坑"小节里的规则不要绕过,每条都实测踩过**。

## 1. 前提(先确认,别试错)

- **双远端**:`origin` = GitHub `chenghaowan/pointtofixcraft`(https);`gitee` = `git@gitee.com:chenghao_wan/pointtofixcraft.git`(SSH)。**Gitee 用户名带下划线 `chenghao_wan`**,与 GitHub 的 `chenghaowan` 不同。
- **本机直连 GitHub 不通**(DNS 污染到 127.0.0.1),必须 FastGithub 正在运行。git(schannel)、PowerShell、`gh` 都能走通;**curl/OpenSSL 不认 FastGithub 的证书**,对 github.com 的 curl 会挂——别用 curl 测 GitHub。
- `gh` 已登录 `chenghaowan`(keyring 凭据),直接用。
- Gitee API 令牌由用户在会话里现给(32 位 hex)。**只进环境变量/内存,不写任何文件、不进 git、不进记忆**。

## 2. 标准发版流程

```bash
# 1) bump 版本(Cargo.toml 的 version)+ cargo build --release
#    bump 单独一个 chore(release) 提交,功能一个 feat 提交(仓库惯例)

# 2) 打包:release exe + 使用说明.txt → dist/pointtofix-craft-portable.zip
#    资产名固定不变!应用内升级器按固定名拼 Gitee 直链(见第 5 节)
powershell -NoProfile -Command "
  \$src='D:\claw.forvia\pointtofixcraft\target\release\pointtofix.exe';
  \$pkg='D:\claw.forvia\pointtofixcraft\dist\ptf-pkg';
  \$zip='D:\claw.forvia\pointtofixcraft\dist\pointtofix-craft-portable.zip';
  if (Test-Path \$pkg) { Remove-Item -Recurse -Force \$pkg };
  New-Item -ItemType Directory \$pkg | Out-Null;
  Copy-Item \$src \"\$pkg\pointtofix.exe\";
  [IO.File]::WriteAllLines(\"\$pkg\使用说明.txt\", @('PointToFix 便携版','','直接双击 pointtofix.exe 运行。'), (New-Object Text.UTF8Encoding \$true));
  if (Test-Path \$zip) { Remove-Item -Force \$zip };
  Compress-Archive -Path \"\$pkg\*\" -DestinationPath \$zip -Force;
  Remove-Item -Recurse -Force \$pkg; (Get-Item \$zip).Length"
# 警告:build-portable.bat 会把 models\ 里的 isnet*.onnx 自动打进包(违反"不带模型"约定)。
# 要用 bat 就先把 isnet*.onnx 挪走,打完还原;或直接用上面的手动打包。

# 3) 提交 + tag + 双推
git tag vX.Y.Z
git push origin main vX.Y.Z
git push gitee main vX.Y.Z

# 4) GitHub release(gh 一步到位,notes 用英文,格式沿用 dist/release-notes-v*.md)
gh release create vX.Y.Z dist/pointtofix-craft-portable.zip \
  --title "PointToFix Craft X.Y.Z" --notes-file dist/release-notes-vX.Y.Z.md

# 5) Gitee release(见第 3 节)

# 6) 双端验证:附件 URL 都会 302 到真实下载地址,核对可达即可
curl -sI "https://gitee.com/chenghao_wan/pointtofixcraft/releases/download/vX.Y.Z/pointtofix-craft-portable.zip"
gh release view vX.Y.Z --repo chenghaowan/pointtofixcraft --json assets --jq '[.assets[] | {name, size}]'
```

## 3. Gitee Release(令牌 API;坑最密集,全部实测)

**为什么不用 Gitee API 查版本**:Releases API 的**匿名 GET 被 WAF 拦,任何 UA 都 403**(浏览器 UA / curl UA / 自定义 UA 全试过,2026-11-07)。HTML 发布页 `https://gitee.com/chenghao_wan/pointtofixcraft/releases` 可达。带 `access_token` 的请求能过 WAF。

**工具选择**:一律 **python 写 UTF-8 JSON 文件 + curl `--data-binary @文件`**。不要用 PowerShell `Invoke-RestMethod` 做 POST/PATCH——同样的 JSON 它会报 `body is invalid`,换 python+curl 就 200(2026-11-07 实测,原因不明,别再试它)。

```bash
TOKEN=<用户当场给的令牌>
UA="Mozilla/5.0 (Windows NT 10.0; Win64; x64) Chrome/120.0"
API="https://gitee.com/api/v5/repos/chenghao_wan/pointtofixcraft"

# 1) 建 release:python 写 JSON(中文字段绝不内联进 curl -d,GBK 控制台会坏 UTF-8)
python -I -c "
import json
open(r'C:/Users/40120/AppData/Local/Temp/ptf_rel.json','w',encoding='utf-8').write(json.dumps({
  'access_token':'$TOKEN','tag_name':'vX.Y.Z','target_commitish':'main',
  'name':'PointToFix Craft X.Y.Z','body':'PointToFix Craft X.Y.Z','prerelease':False}, ensure_ascii=False))"
#    ★ POST 必须带 target_commitish,缺了 400
curl -sS -X POST -H "User-Agent: $UA" -H "Content-Type: application/json; charset=utf-8" \
  --data-binary "@C:/Users/40120/AppData/Local/Temp/ptf_rel.json" "$API/releases"
#    响应里取 "id"(release 数字 id,不是 tag 名)

# 2) PATCH 完整 release notes:★ 必须同时带 tag_name + name + body,缺一个就 400
#    (报错文案 "body is invalid" 有误导性,不是 body 内容的问题)
python -I -c "
import json
notes = open(r'D:/claw.forvia/pointtofixcraft/dist/release-notes-vX.Y.Z.md', encoding='utf-8').read()
open(r'C:/Users/40120/AppData/Local/Temp/ptf_patch.json','w',encoding='utf-8').write(json.dumps({
  'access_token':'$TOKEN','tag_name':'vX.Y.Z','name':'PointToFix Craft X.Y.Z',
  'body':notes,'prerelease':False}, ensure_ascii=False))"
curl -sS -X PATCH -H "User-Agent: $UA" -H "Content-Type: application/json; charset=utf-8" \
  --data-binary "@C:/Users/40120/AppData/Local/Temp/ptf_patch.json" "$API/releases/<id>"   # 200 = 成功

# 3) 传附件:成功响应里就有 name/size/browser_download_url,核对 size 与本地一致
curl -sS -X POST -H "User-Agent: $UA" \
  -F "file=@D:/claw.forvia/pointtofixcraft/dist/pointtofix-craft-portable.zip;type=application/zip" \
  "$API/releases/<id>/attach_files?access_token=$TOKEN"

# 4) 删 release(换资产时用):204 = 成功
curl -sS -X DELETE -H "User-Agent: $UA" "$API/releases/<id>?access_token=$TOKEN"

# 5) 清理含令牌的临时 JSON
rm -f C:/Users/40120/AppData/Local/Temp/ptf_rel.json C:/Users/40120/AppData/Local/Temp/ptf_patch.json
```

其它:python 打印中文响应前先 `sys.stdout.reconfigure(encoding="utf-8")`;curl 只认 `C:/` 风格路径,不认 Git Bash 的 `/c/`;同名附件重复传会产生歧义——**换资产时删整个 release 重建**,别只追加附件。

## 4. 发版后发现代码要修(tag 已打、资产已传)

1. 修完重新 `cargo build --release` + 重新打包
2. `git tag -f vX.Y.Z <修复提交>` → `git push -f origin vX.Y.Z` → `git push -f gitee vX.Y.Z`
3. GitHub:`gh release upload vX.Y.Z dist/pointtofix-craft-portable.zip --clobber`
4. Gitee:DELETE 旧 release → 按第 3 节重建(保证 tag=资产一致)

## 5. 应用内升级器契约(v0.3.2+,src/update.rs)

发版前确认这两条,否则升级器断链:

- **资产名必须精确**是 `pointtofix-craft-portable.zip`——升级器按 `https://gitee.com/chenghao_wan/pointtofixcraft/releases/download/<tag>/pointtofix-craft-portable.zip` 拼 Gitee 直链(下载首选),GitHub 附件 URL 兜底。
- **检查**:解析 Gitee HTML 发布页取最新 `releases/tag/v*`(API 匿名 GET 被 WAF 403),GitHub API 兜底;**下载**:Gitee 直链优先(GitHub 附件经代理下载大文件会停滞且不报错,已加 15s 接收超时触发换源)。
- 发完版真机跑一遍:旧版本启动 → 自动检查弹 toast → 设置→关于 → 下载 → 立即升级并重启。首次发"带升级器的版本"之后的下一个版本才能验证完整链路。

## 6. 为什么是这套流程(背景,别改约定)

- exe/zip 永不进 git(dist/、target/ 已 ignore),只走 Release 附件;仓库无 CI,发布全部本地构建。
- 便携包 = 单 exe + 使用说明,不带模型、不带 DLL(v0.3.1 起 onnxruntime.dll 不再内嵌,推理走 rs-aiprivaderCraft)。
- 加 Gitee 是为了国内访问不依赖 FastGithub;GitHub 每次提交后双推是既定约定。
