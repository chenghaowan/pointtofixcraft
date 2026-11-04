# PointToFix 截图库 schema(外部接入契约)

PointToFix 把每次「保存」的截图与其 OCR 文本写入一个 SQLite 数据库,路径:

```
%APPDATA%\pointtofix\gallery.db
```

图片文件与其缩略图在:

```
%APPDATA%\pointtofix\gallery\*.png|*.webp
%APPDATA%\pointtofix\gallery\thumbs\*.png
```

数据库为 **WAL 模式**,第三方软件(如 RAG 管道)可以只读连接并发访问,不影响 GUI 写入。写入方只有 PointToFix 自身。

## 表结构

### shots(主表)

| 列 | 类型 | 说明 |
|---|---|---|
| `id` | INTEGER PRIMARY KEY AUTOINCREMENT | **稳定外键**,外部系统引用它关联图片 |
| `created_at` | INTEGER NOT NULL | 保存时刻,unix 毫秒(UTC,显示时自行转本地时区) |
| `file_path` | TEXT NOT NULL | 图片路径,**相对** `%APPDATA%\pointtofix\gallery\`,分隔符为 `/`(整目录可搬家/同步) |
| `thumb_path` | TEXT | 缩略图相对路径,可为 NULL |
| `format` | TEXT NOT NULL | `png` 或 `webp` |
| `width` / `height` | INTEGER NOT NULL | 像素尺寸(切图/分辨率过滤用) |
| `file_size` | INTEGER NOT NULL | 字节 |
| `content_hash` | TEXT | 图片字节流的 SHA-256(hex),**去重键** |
| `app_name` | TEXT | 截图瞬间前台应用窗口标题(「来源」过滤维度),可为 NULL |
| `ocr_text` | TEXT NOT NULL DEFAULT '' | OCR 全文,**干净无序号**;FTS 索引对象 |
| `ocr_lines` | TEXT | JSON 数组,每行一个文本块: `[{"no":1,"text":"登录","box":[x,y,w,h]}, …]`。`no` 为阅读顺序编号(画在截图上的 ①②③ 圆标),`box` 为相对图片的像素矩形 |
| `ocr_status` | TEXT NOT NULL DEFAULT 'pending' | `pending`(OCR 未完成,可能回写)/ `done` / `failed` / `off`(无选区保存,未跑 OCR) |

### shots_fts(FTS5 虚拟表,外部内容表)

```sql
CREATE VIRTUAL TABLE shots_fts USING fts5(
  ocr_text, app_name,
  content='shots', content_rowid='id', tokenize='trigram'
);
```

- 与主表通过 **触发器**(`shots_ai`/`shots_ad`/`shots_au`)自动同步,文本不落两份;
- tokenizer 为 SQLite 内建 **trigram**(≥3.34 自带):对中/英/日混排做**子串**匹配,外部客户端无需注册任何自定义分词器;
- trigram 要求查询 **≥3 个字符**;更短的查询请对 `shots.ocr_text` 用 `LIKE '%q%'`(库为个人规模,全表扫描毫秒级)。

## 查询示例

```sql
-- 全文检索(≥3 字符)
SELECT s.* FROM shots s
WHERE s.id IN (SELECT rowid FROM shots_fts WHERE shots_fts MATCH '"审批流程"')
ORDER BY s.created_at DESC;

-- 短查询回退 LIKE
SELECT * FROM shots WHERE ocr_text LIKE '%审批%' ESCAPE '\';

-- 按来源 + 时间范围
SELECT * FROM shots
WHERE app_name LIKE '%微信%' AND created_at >= 1762252800000;

-- 按内容指纹去重
SELECT * FROM shots WHERE content_hash = ?;
```

## 接入建议(RAG)

1. 增量拉取:`SELECT * FROM shots WHERE id > ?last_seen ORDER BY id`;
2. 图片路径 = `%APPDATA%\pointtofix\gallery\` + `file_path`(把 `/` 还原为系统分隔符);
3. `ocr_text` 直接作为该图的文本切片来源;`ocr_lines` 可按文本块切分并携带坐标;
4. 删除事件:PointToFix 画廊删除会同时删行与文件,外部以 `id` 查不到即视为已删;
5. 语义检索:若需 embedding,建议外部建独立库以 `shots.id` 为外键,或新增表 `embeddings(id INTEGER PRIMARY KEY, vec BLOB)`(按 id 关联,PointToFix 不写入、不迁移该表);
6. `user_version` 当前为 `1`,未来 schema 演进以此为准。

## 模型文件(可选能力)

AI 去背与 OCR 的模型文件也在 `%APPDATA%\pointtofix\models\`(或程序目录 `models\`),缺失时对应功能自动降级,不影响数据库与画廊。
