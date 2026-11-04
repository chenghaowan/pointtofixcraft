---
name: pointtofix
description: 查询/检索 PointToFix 截图库(SQLite+FTS5,含 OCR 文本),或用无头 CLI 驱动截图→OCR→入库。适用于用户要求"找一张之前的截图"、"把屏幕某区域截图并识别文字"、"截图库里搜××"等场景。
---

# PointToFix 截图库接入

PointToFix 是一款 Windows 屏幕标注工具。它把用户保存的每张截图(含 OCR 文本、来源应用、时间)存入本地 SQLite 库,并提供一个无头 CLI。你可以**直接读库检索**,也可以**调用 CLI 完成截图**。两种方式都不需要 MCP——普通 sqlite3 / 子进程即可。

## 1. 数据在哪

- 数据库:`%APPDATA%\pointtofix\gallery.db`(WAL 模式,可只读并发访问)
- 图片目录:`%APPDATA%\pointtofix\gallery\`(`file_path` 是相对此目录的路径,`/` 分隔)
- 完整字段契约:仓库内 `docs/db-schema.md`(先读它再写复杂查询)

关键列:`id`(稳定外键)、`created_at`(unix 毫秒)、`ocr_text`(干净全文)、`ocr_lines`(JSON,带 ①②③ 序号与文本框坐标)、`app_name`(来源应用窗口标题)、`content_hash`(SHA-256 去重)、`ocr_status`(done/pending/failed/off)。

## 2. 检索(只读,随时可做)

用 `sqlite3` CLI 或任何 SQLite 驱动直连。注意两点:

- **≥3 字符**的查询走 FTS(子串匹配,中英日通吃):
  ```sql
  SELECT s.id, s.created_at, s.app_name, s.file_path, s.ocr_text
  FROM shots s
  WHERE s.id IN (SELECT rowid FROM shots_fts WHERE shots_fts MATCH '"查询词"')
  ORDER BY s.created_at DESC LIMIT 20;
  ```
- **<3 字符**(如中文两字词)FTS 不命中,改用 LIKE:
  ```sql
  SELECT * FROM shots WHERE ocr_text LIKE '%查询%' ESCAPE '\' ORDER BY created_at DESC;
  ```

结果里的图片绝对路径 = `%APPDATA%\pointtofix\gallery\` + `file_path`(把 `/` 换成 `\`)。

## 3. 截图(CLI 无头模式)

```bash
# 截取屏幕区域(虚拟桌面物理像素 x,y,w,h),存入画廊并写库,输出 JSON 回执
pointtofix.exe --capture 100,100,800,600 --ocr --json
# 回执:{"id":42,"file":"C:\\...\\PointToFix_20261104_153012.png","ocr":[{"no":1,"text":"..."}]}

# 只落一个临时文件(不进画廊不写库)
pointtofix.exe --capture 0,0,1920,1080 --out D:\\tmp\\shot.png
```

- `--ocr` 需要 OCR 模型已就位(det/rec/词典);缺失时报错、退出码非 0。
- 退出码:0 成功;1 失败;2 参数错误。
- 坐标是**物理像素**;多显示器时主屏左上角为 (0,0),负坐标合法(左侧/上方副屏)。
- 没有模型时 `--ocr` 会失败,但**不带 `--ocr` 的截图照常可用**。

## 4. 边界与注意

- 这是**个人截图库**:不要写库、不要删行;写操作只属于 GUI。
- `ocr_status='pending'` 的行 OCR 可能稍后回写,检索时可按需过滤。
- 用户在图片上画的标注会烙在图片像素里;`ocr_text` 识别的是**标注前**的原始画面文字,与图片像素可能不同(标注文字不在其中)。
- 全部能力仅 Windows。

## 5. 调试钩子(仅自动化测试用,别在用户会话里用)

启动 GUI 前设置环境变量可驱动自检:`POINTTOFIX_START_ACTIVE=1`(直接进标注)、`POINTTOFIX_AUTOSAVE=1`(2 秒后自动保存)、`POINTTOFIX_STAMPTEST=1`(自动盖章+存图)。产物落到画廊目录,可用第 2 节的查询验证。
