---
name: 钉钉文档
description: 操作钉钉在线文档、表格、多维表格、知识库和智能填表（问卷/表单设计），包括内容编辑、表格读写、知识库管理、文档搜索
triggers:
  - 钉钉文档
  - 钉钉表格
  - 在线文档
  - 在线表格
  - 知识库
  - 搜索文档
  - 写文档
  - 写表格
  - 读文档
  - 读表格
  - workbook
  - 云文档
  - 多维表格
  - AI表格
  - notable
  - 智能填表
  - 表单
  - 新建表单
  - 填表
  - 问卷
requires: []
default: true
---

# 钉钉文档

操作钉钉在线文档、表格、多维表格和知识库。四个工具脚本分别负责：

- `tools/dingtalk_doc.py` — 文档编辑（覆写、块操作、追加、上传图片）
- `tools/dingtalk_sheet.py` — 普通表格读写（工作表管理、行列操作、单元格区域读写）
- `tools/dingtalk_notable.py` — 多维表格 / AI 表格（数据表/字段/记录 CRUD + 附件上传）
- `tools/dingtalk_wiki.py` — 知识库管理（知识库列表、创建节点、搜索文档、权限查询）

授权管理：`tools/dingtalk_auth.py`

## 安全约束（必须遵守）

1. **写操作前权限预检**：每次用户指令（如"帮我写文档"、"把数据填到表格"）开始执行前，先调一次 `dingtalk_wiki.py query-permissions --dentry-uuid <id>` 确认操作人角色 >= EDITOR。这是每次用户指令一次，不是每个 API 调用都查
2. **禁止删除文件和文件夹**：不允许调用任何删除文档或文件夹的接口
3. **overwrite-doc 必须向用户确认**：覆写会替换文档全部内容，执行前必须明确告知用户并获得确认
4. **禁止盲写**：写入表格前必须先读取表头和现有数据（list-sheets → get-range），确认列结构匹配后再写入
5. **operatorId 归属**：所有操作必须使用当前用户的身份，不得用他人身份操作

## 身份验证与授权流程

所有钉钉 API 调用通过 Bot 侧代理自动完成身份验证，**无需手动传递任何凭据或 operatorId**。

如果用户首次使用且尚未绑定钉钉身份，工具会返回包含 `auth_url` 的错误。收到此错误时：
1. 将 `auth_url` 链接发送给用户（告知"请在浏览器中打开此链接，用钉钉扫码完成授权"）
2. 用户完成授权后说「继续」，工具即可正常执行

## 文档操作 — dingtalk_doc.py

```bash
# 读取文档内容（推荐：自动提取段落/标题/列表/引用的文本）
python tools/dingtalk_doc.py read-doc --doc-id <id>
python tools/dingtalk_doc.py read-doc --doc-id <id> --format json

# 覆写文档（⚠️ 破坏性操作，必须先向用户确认）
python tools/dingtalk_doc.py overwrite-doc --doc-id <id> --content "# 标题\n\n内容"
python tools/dingtalk_doc.py overwrite-doc --doc-id <id> --content-file /path/to/file.md

# 查询块元素列表（低层级，用于获取 block-id 做后续 CRUD）
python tools/dingtalk_doc.py get-blocks --doc-id <id>
python tools/dingtalk_doc.py get-blocks --doc-id <id> --format summary

# 插入 Markdown 内容（追加到文档末尾或指定位置）
python tools/dingtalk_doc.py insert-content --doc-id <id> --content "**新内容**\n"
python tools/dingtalk_doc.py insert-content --doc-id <id> --content-file /path/to/content.md --index 3

# 插入块元素（段落/标题）
python tools/dingtalk_doc.py insert-block --doc-id <id> --block-type paragraph --text "新段落内容"
python tools/dingtalk_doc.py insert-block --doc-id <id> --block-type heading --text "新标题" --level 2

# 更新块元素
python tools/dingtalk_doc.py update-block --doc-id <id> --block-id <bid> --block-type paragraph --text "更新后的内容"

# 删除块元素
python tools/dingtalk_doc.py delete-block --doc-id <id> --block-id <bid>

# 在段落末尾追加纯文本
python tools/dingtalk_doc.py append-text --doc-id <id> --block-id <bid> --text "追加的文字"

# 在段落末尾追加带样式的行内元素
python tools/dingtalk_doc.py append-element --doc-id <id> --block-id <bid> --text "粗体文字" --style '{"bold":true}'

# 上传图片到文档（获取URL + 上传 + 返回资源ID）
python tools/dingtalk_doc.py upload-image --doc-id <id> --file /path/to/image.png
```

### 文档读取能力边界

`read-doc` 通过 blocks API 提取文本，存在以下限制：

| 块类型 | 能否读取 | 备注 |
|--------|---------|------|
| 段落 `paragraph`、标题 `heading` | 仅纯文本 | **超链接 URL 丢失**，只保留链接文字 |
| 无序列表、有序列表、引用 | 可读纯文本 | 列表层级信息丢失 |
| 表格 `table` | 不可读 | 仅显示行列数占位符 |
| 代码块、待办、高亮块、分栏 | 不可读 | API 映射为 unknown |

**核心限制**：blocks API 的读写能力不对称——写入支持富文本（超链接、样式等），但查询只返回拍平的纯文本。

### 推荐流程

```
读取文档内容：read-doc（获取纯文本，超链接 URL / 表格内容 / 代码块等会丢失，这是钉钉 API 限制）

需要块级操作（更新/删除特定块）→ 先 get-blocks 获取块 ID

编辑流程：
1. read-doc → 了解文档当前内容
2. 根据需求选择操作方式：
   - 整篇替换 → overwrite-doc（需确认）
   - 追加内容 → insert-content / insert-block
   - 修改现有内容 → get-blocks 获取块 ID → update-block
   - 段落内追加 → append-text / append-element
3. 操作完成后可再次 read-doc 验证结果
```

### 参数说明

| 参数 | 说明 |
|------|------|
| `--doc-id` | 文档 ID（docKey 或 dentryUuid，从钉钉 URL 或搜索结果获取） |
| `--block-id` | 块元素 ID（从 get-blocks 获取） |
| `--block-type` | 块类型：paragraph / heading 等 |
| `--text` | 文本内容 |
| `--level` | 标题级别 1-6（heading 用） |
| `--index` | 插入位置索引（不指定则追加到末尾） |
| `--style` | 样式 JSON，如 `{"bold":true}` / `{"italic":true}` |
| `--element-type` | 行内元素类型（默认 text） |

## 表格读写 — dingtalk_sheet.py

```bash
# 列出工作表
python tools/dingtalk_sheet.py list-sheets --workbook-id <id>
python tools/dingtalk_sheet.py list-sheets --workbook-id <id> --format markdown

# 获取单个工作表详情
python tools/dingtalk_sheet.py get-sheet --workbook-id <id> --sheet-id <id>

# 读取区域数据
python tools/dingtalk_sheet.py get-range --workbook-id <id> --sheet-id <id> --range "A1:Z10"
python tools/dingtalk_sheet.py get-range --workbook-id <id> --sheet-id <id> --range "A1:Z10" --format markdown
# 筛选返回字段（减少数据量）
python tools/dingtalk_sheet.py get-range --workbook-id <id> --sheet-id <id> --range "A1:Z10" --select "values,backgroundColors,fontWeights"

# 创建新工作表
python tools/dingtalk_sheet.py create-sheet --workbook-id <id> --name "新Sheet"

# 追加行（最常用的写入方式）
python tools/dingtalk_sheet.py append-rows --workbook-id <id> --sheet-id <id> --data '[["val1","val2"]]'
python tools/dingtalk_sheet.py append-rows --workbook-id <id> --sheet-id <id> --data-file /tmp/data.json

# 更新指定区域（值 + 样式，所有参数均可选，至少传一个）
python tools/dingtalk_sheet.py update-range --workbook-id <id> --sheet-id <id> --range "A1:C2" --data '[["a","b","c"]]'
# 带样式写入：背景色 + 加粗 + 字号 + 水平对齐
python tools/dingtalk_sheet.py update-range --workbook-id <id> --sheet-id <id> --range "A1:C1" \
  --data '[["标题A","标题B","标题C"]]' \
  --background-colors '[["#0071c1","#0071c1","#0071c1"]]' \
  --font-weights '[["bold","bold","bold"]]' \
  --font-sizes '[[14,14,14]]' \
  --h-aligns '[["center","center","center"]]'
# 仅设样式不改值
python tools/dingtalk_sheet.py update-range --workbook-id <id> --sheet-id <id> --range "A1:C1" \
  --background-colors '[["#ff0000","#00ff00","#0000ff"]]'

# 自动调整行高（根据字号自适应）
python tools/dingtalk_sheet.py autofit-rows --workbook-id <id> --sheet-id <id> --row 0 --count 10 --font-size 14

# 查找单元格（从指定位置向后查找匹配文本）
python tools/dingtalk_sheet.py find-next --workbook-id <id> --sheet-id <id> --range "A1" --text "关键词"
python tools/dingtalk_sheet.py find-next --workbook-id <id> --sheet-id <id> --range "A1" --text "正则.*" --use-regexp --scope "A1:Z100"

# 插入行/列
python tools/dingtalk_sheet.py insert-rows --workbook-id <id> --sheet-id <id> --row 5 --count 3
python tools/dingtalk_sheet.py insert-columns --workbook-id <id> --sheet-id <id> --column 2 --count 1

# 删除行/列
python tools/dingtalk_sheet.py delete-rows --workbook-id <id> --sheet-id <id> --row 5 --count 3
python tools/dingtalk_sheet.py delete-columns --workbook-id <id> --sheet-id <id> --column 2 --count 1

# 清除区域数据（保留格式）
python tools/dingtalk_sheet.py clear-data --workbook-id <id> --sheet-id <id> --range "A2:Z100"

# 清除区域所有内容（含格式）
python tools/dingtalk_sheet.py clear-all --workbook-id <id> --sheet-id <id> --range "A2:Z100"

# 更新工作表属性（重命名/冻结行列/隐藏）
python tools/dingtalk_sheet.py update-sheet --workbook-id <id> --sheet-id <id> --name "新名称"
python tools/dingtalk_sheet.py update-sheet --workbook-id <id> --sheet-id <id> --frozen-rows 1 --frozen-cols 1
python tools/dingtalk_sheet.py update-sheet --workbook-id <id> --sheet-id <id> --visibility hidden

# 批量设置列宽/行高
python tools/dingtalk_sheet.py set-columns-width --workbook-id <id> --sheet-id <id> --column 0 --count 5 --width 120
python tools/dingtalk_sheet.py set-rows-height --workbook-id <id> --sheet-id <id> --row 0 --count 3 --height 40

# 合并单元格
python tools/dingtalk_sheet.py merge-cells --workbook-id <id> --sheet-id <id> --range "A1:B2"
python tools/dingtalk_sheet.py merge-cells --workbook-id <id> --sheet-id <id> --range "A1:D1" --merge-type mergeRows

# 插入/删除下拉列表
python tools/dingtalk_sheet.py insert-dropdown --workbook-id <id> --sheet-id <id> --range "C1:C10" --options '[{"value":"是","color":"#00ff00"},{"value":"否","color":"#ff0000"}]'
python tools/dingtalk_sheet.py delete-dropdown --workbook-id <id> --sheet-id <id> --range "C1:C10"

# 查找所有匹配单元格（支持正则、聚合地址）
python tools/dingtalk_sheet.py find-all --workbook-id <id> --sheet-id <id> --text "关键词" --select a1Notation
python tools/dingtalk_sheet.py find-all --workbook-id <id> --sheet-id <id> --text "\\d+" --use-regexp --scope "A:A"

# 创建条件格式规则
python tools/dingtalk_sheet.py create-conditional-format --workbook-id <id> --sheet-id <id> --ranges '["A1:A100"]' --number-op greater --value1 90 --bg-color "#00ff00"
python tools/dingtalk_sheet.py create-conditional-format --workbook-id <id> --sheet-id <id> --ranges '["B1:B100"]' --duplicate --bg-color "#ffcc00"

```

### 表格写入推荐流程

```
1. list-sheets → 了解工作表结构（id、名称、行列数）
2. get-range --range "A1:Z1" → 读表头，了解列含义
3. get-range --range "A1:Z5" → 读现有数据，了解格式
4. append-rows 追加 或 update-range 覆盖写入
5. update-range --background-colors/--font-weights/... → 设置样式
6. autofit-rows → 自动调整行高适配内容
```

### update-range 样式参数

所有二维数组参数的维度必须与 `--range` 的行列数匹配。所有参数均可选，至少传一个。

| 参数 | 类型 | 说明 |
|------|------|------|
| `--data` | `[[String]]` | 单元格值 |
| `--background-colors` | `[[String]]` | 十六进制色值，如 `"#0071c1"` |
| `--font-sizes` | `[[Integer]]` | 字号，如 `10` / `14` / `20` |
| `--font-weights` | `[[String]]` | 加粗：`"bold"` / `"normal"`（官方文档未列出，实测可用且 get-range 可读回） |
| `--h-aligns` | `[[String]]` | 水平对齐：`left` / `center` / `right` / `general` |
| `--v-aligns` | `[[String]]` | 垂直对齐：`top` / `middle` / `bottom` |
| `--hyperlinks` | `[[Object]]` | 超链接（见下方详情） |
| `--number-format` | `String` | 数字格式（见下方详情） |

**hyperlinks 格式**：每个元素 `{"type":"...","link":"...","text":"..."}`

| type | link 示例 | 用途 |
|------|----------|------|
| `path` | `https://www.dingtalk.com` | 外部 URL |
| `sheet` | `Sheet2` | 跳转到其他工作表 |
| `range` | `Sheet2!A4` | 跳转到指定单元格 |

**numberFormat 可选值**：

| 格式串 | 示例 | 格式串 | 示例 |
|--------|------|--------|------|
| `General` | 常规 | `@` | 文本 |
| `#,##0` | 1,234 | `#,##0.00` | 1,234.56 |
| `0%` | 12% | `0.00%` | 12.34% |
| `0.00E+00` | 1.01E+03 | `¥#,##0` | ¥1,234 |
| `¥#,##0.00` | ¥1,234.56 | `$#,##0` | $1,234 |
| `$#,##0.00` | $1,234.56 | `yyyy/m/d` | 2022/1/1 |
| `yyyy年m月d日` | 2022年1月1日 | `yyyy年m月` | 2022年1月 |
| `hh:mm:ss` | 00:00:00 | `yyyy/m/d hh:mm:ss` | 2022/1/1 00:00:00 |

### get-range --select 可用字段

`values`, `formulas`, `displayValues`, `backgroundColors`, `fontSizes`, `fontWeights`, `horizontalAlignments`, `verticalAlignments`, `hyperlinks`

建议指定 `--select` 以提高性能，避免返回全量数据导致超时。

### set-rows-visibility / set-columns-visibility 参数

| 参数 | 说明 |
|------|------|
| `--row` / `--column` | 起始行号/列号（0-based） |
| `--count` | 行数/列数 |
| `--visibility` | `visible` 或 `hidden` |

### autofit-rows 参数

| 参数 | 说明 |
|------|------|
| `--row` | 起始行号（0-based，第一行=0） |
| `--count` | 需调整的行数 |
| `--font-size` | 字号大小（API 字段名 `fontWidth`） |

### update-sheet 参数

| 参数 | 说明 |
|------|------|
| `--name` | 新的工作表名 |
| `--frozen-rows` | 冻结至第 N 行（从1开始，0=不冻结） |
| `--frozen-cols` | 冻结至第 N 列（从1开始，0=不冻结） |
| `--visibility` | `visible` 或 `hidden` |

### set-columns-width / set-rows-height 参数

| 参数 | 说明 |
|------|------|
| `--column` / `--row` | 起始列号/行号（0-based） |
| `--count` | 连续列/行数 |
| `--width` / `--height` | 像素值 |

### merge-cells 参数

| 参数 | 说明 |
|------|------|
| `--range` | 合并区域（如 `A1:B2`） |
| `--merge-type` | `mergeAll`（默认）/ `mergeRows` / `mergeColumns` |

### insert-dropdown 参数

| 参数 | 说明 |
|------|------|
| `--range` | 应用下拉列表的区域 |
| `--options` | JSON 数组：`[{"value":"选项名","color":"#ff0000"}]`，color 可选 |

### find-all 参数

| 参数 | 说明 |
|------|------|
| `--text` | 查找文本（`--use-regexp` 时为正则模式） |
| `--select` | 筛选返回字段（如 `a1Notation,values`） |
| `--scope` | 搜索范围（A1 表示法） |
| `--no-union` | 不聚合地址（默认聚合） |
| 其余 | `--match-case` / `--match-entire-cell` / `--use-regexp` / `--match-formula` / `--include-hidden` |

### create-conditional-format 参数

| 参数 | 说明 |
|------|------|
| `--ranges` | JSON 数组，如 `["A1:B10"]` |
| `--duplicate` | 重复值高亮规则 |
| `--number-op` | 数字比较：equal/not-equal/greater/greater-equal/less/less-equal/between/not-between |
| `--value1` / `--value2` | 比较值（between/not-between 时需要 value2） |
| `--bg-color` | 背景色（如 `#ff0000`） |
| `--font-color` | 字体色 |

### find-next 参数

| 参数 | 说明 |
|------|------|
| `--range` | 搜索起始位置（不含该单元格，从其之后开始查找） |
| `--text` | 查找文本 |
| `--scope` | 搜索范围（A1 表示法，如 `A1:E10` 或 `A:A`），与起始位置取交集 |
| `--match-case` | 区分大小写 |
| `--match-entire-cell` | 全单元格匹配 |
| `--use-regexp` | 正则匹配 |
| `--match-formula` | 搜索公式文本 |
| `--include-hidden` | 包含隐藏单元格 |

### 通用参数

| 参数 | 说明 |
|------|------|
| `--workbook-id` | 表格 ID（dentryUuid，从钉钉 URL 的 `/nodes/<id>` 提取） |
| `--sheet-id` | 工作表 ID 或标题（从 list-sheets 获取） |
| `--range` | 单元格区域，A1 表示法（如 `A1:C10`） |
| `--data` / `--data-file` | 二维 JSON 数组（内联或文件） |
| `--row` / `--column` | 行/列号（0-based，第一行=0） |
| `--count` | 操作的行/列数 |
| `--format` | 输出格式：json / markdown |

### 数据格式

写入数据必须是二维 JSON 数组：`[["行1列1","行1列2"],["行2列1","行2列2"]]`

## 知识库管理 — dingtalk_wiki.py

```bash
# 获取知识库列表
python tools/dingtalk_wiki.py list-wikis
python tools/dingtalk_wiki.py list-wikis --format markdown

# 获取知识库详情
python tools/dingtalk_wiki.py get-wiki --workspace-id <id>

# 获取我的文档知识库（个人空间）
python tools/dingtalk_wiki.py my-wiki

# 创建文档/表格/文件夹
python tools/dingtalk_wiki.py create-node --workspace-id <id> --name "新文档" --doc-type DOC
python tools/dingtalk_wiki.py create-node --workspace-id <id> --name "新表格" --doc-type WORKBOOK
python tools/dingtalk_wiki.py create-node --workspace-id <id> --name "新多维表格" --doc-type NOTABLE
python tools/dingtalk_wiki.py create-node --workspace-id <id> --name "子文件夹" --doc-type FOLDER
python tools/dingtalk_wiki.py create-node --workspace-id <id> --name "文档" --doc-type DOC --parent-node-id <folderId>

# 搜索文档（全文搜索，搜索范围为操作人可见的全部文档）
python tools/dingtalk_wiki.py search-docs --keyword "周报"
python tools/dingtalk_wiki.py search-docs --keyword "项目计划" --format markdown

# 查询文档权限列表（写操作前必须先调此接口检查权限）
python tools/dingtalk_wiki.py query-permissions --dentry-uuid <id>
python tools/dingtalk_wiki.py query-permissions --dentry-uuid <id> --format markdown
python tools/dingtalk_wiki.py query-permissions --dentry-uuid <id> --filter-role-ids OWNER EDITOR
```

### 如何定位目标文档/表格

用户需要操作某个文档或表格时，按以下优先级获取 ID：

1. **用户直接提供知识库链接**（`/i/nodes/{id}`）：`/nodes/` 后的 ID 就是 `dentryUuid`，直接用作 `--doc-id` 或 `--workbook-id`
2. **用户提供通用空间链接**（`/spreadsheetv2/`、`/core/` 等）：URL 的 `docKey` 查询参数是 API 所需的 ID，用作 `--workbook-id` 或 `--doc-id`。**不要用 URL 路径中的 ID 或 `dentryKey` 参数**，那是另一个标识，当 workbook-id 会 404。同时用 `search-docs` 按文档名搜索获取 `dentryUuid`，用于后续生成标准链接（见"链接生成规则"）
3. **用户提供文档标题**：用 `search-docs --keyword` 搜索，返回结果中的 `dentryUuid` 可直接用于 API 调用
4. **需要新建文档**：用 `create-node` 在指定知识库中创建

**重要 — 搜索的局限性：**
- `search-docs` 是全局全文模糊搜索，会匹配标题和正文中包含关键词的所有可见文档
- 常见词（如"测试"、"报告"）可能返回大量不相关结果
- 无法按知识库过滤，搜索范围是用户可见的全部文档
- 新建文档有索引延迟（几分钟到十几分钟），刚创建的文档可能搜不到

**因此，当用户未提供文档 ID 且描述模糊时，应主动引导：**
- "请提供文档链接或文档 ID，我可以直接操作"
- "如果没有链接，请告诉我文档的完整标题，我帮你搜索"
- 搜索结果有多个匹配时，列出候选让用户确认，不要自行猜测

### 参数说明

| 参数 | 说明 |
|------|------|
| `--workspace-id` | 知识库 ID |
| `--name` | 节点名称 |
| `--doc-type` | 类型：DOC / WORKBOOK / NOTABLE(多维表格) / FOLDER |
| `--parent-node-id` | 父节点 ID（不填则在根目录） |
| `--keyword` | 搜索关键词 |
| `--dentry-uuid` | 文档 dentryUuid（用于权限查询） |
| `--filter-role-ids` | 过滤角色（如 OWNER EDITOR） |

### 权限角色说明

| 角色 | 说明 |
|------|------|
| OWNER | 所有者 |
| MANAGER | 管理者 |
| EDITOR | 编辑者（写操作最低要求） |
| DOWNLOADER | 下载者 |
| READER | 阅读者 |

### ID 类型对照

| ID | 长度 | 说明 | 获取方式 | 用途 |
|----|------|------|---------|------|
| `dentryUuid` | 32 字符 | 文档全局唯一 ID | search-docs / create-node / 知识库 URL 的 `/nodes/` 后 | `--doc-id`、`--workbook-id`、`--dentry-uuid`、构造文档链接 |
| `docKey` | 16 字符 | 文档内部 key | 通用空间 URL 的 `docKey` 查询参数 | `--doc-id`、`--workbook-id` |
| `dentryKey` | 16 字符 | URL 路径中的短标识 | 通用空间 URL 路径 `/spreadsheetv2/{dentryKey}/...` | **不能**当 workbook-id，会 404 |
| `workspaceId` | — | 知识库 ID | list-wikis / my-wiki | 知识库操作 |
| `sheetId` | — | 工作表 ID | dingtalk_sheet.py list-sheets | 工作表操作 |

知识库文档：`dentryUuid` = `docKey` = `dentryKey`（三者相同，都是 32 字符）
通用空间文档：三者**不同**，`dentryKey`（16 字符）不能当 workbook-id

### 链接生成规则

给用户返回文档链接时，统一用 `https://alidocs.dingtalk.com/i/nodes/{dentryUuid}` 格式。

- 用户提供的是知识库链接（`/i/nodes/`）→ 直接复用原链接即可
- 用户提供的是通用空间链接（`/spreadsheetv2/`、`/core/` 等），或只提供了 `docKey` → 用 `search-docs` 按文档名搜索，取返回结果的 `dentryUuid`（32 字符）拼接 `/i/nodes/{dentryUuid}`
- `create-node` 创建的文档 → 返回值中的 ID 就是 `dentryUuid`，直接拼接

**禁止**：用通用空间 URL 中的 `dentryKey`（16 字符）拼 `/i/nodes/` 链接——会打不开。

### 限制

- **不支持移动文档**：钉钉 API 没有 move 接口，已有文档无法移动到其他文件夹
- **不支持列出文件夹内容**：未开通 Wiki.Node.Read 权限，无法遍历文件夹

## 多维表格（AI 表格）— dingtalk_notable.py

多维表格（Notable / `.able` 文件）使用独立的 API 命名空间 `/v1.0/notable`，与普通表格（`dingtalk_sheet.py`）完全不同。多维表格基于字段（Field）+ 记录（Record）模型，类似数据库表。

### 核心概念

| 概念 | 说明 |
|------|------|
| `base_id` | 多维表格文件的 nodeId（全局唯一），从 URL 提取：`https://alidocs.dingtalk.com/i/nodes/<base_id>?...` |
| `Sheet` | 数据表，一个 Base 内可包含多个数据表（至少 1 个） |
| `Field` | 字段 = 列定义，有 id / name / type / property。每个 Sheet 第一列为主字段，不可删除 |
| `Record` | 记录 = 数据行，用 `{"字段名": 值}` 格式读写 |

标识唯一性：`baseId` 全局唯一；`sheetId` / `fieldId` / `recordId` 仅在当前 base 内唯一，跨文档不可共用。

入参规则：Sheet 相关接口支持传 `sheetId` 或 `sheetName`；Field 相关接口支持传 `fieldId` 或 `fieldName`；Record 接口仅支持 `recordId`。

### 数据表操作

```bash
# 获取所有数据表
python tools/dingtalk_notable.py list-sheets --base-id <id>
python tools/dingtalk_notable.py list-sheets --base-id <id> --format markdown

# 获取单个数据表
python tools/dingtalk_notable.py get-sheet --base-id <id> --sheet-id <id>

# 创建数据表（可选带初始字段）
python tools/dingtalk_notable.py create-sheet --base-id <id> --name "任务表"
python tools/dingtalk_notable.py create-sheet --base-id <id> --name "任务表" --fields '[{"name":"标题","type":"text"},{"name":"状态","type":"singleSelect","property":{"choices":[{"name":"待办"},{"name":"进行中"},{"name":"完成"}]}}]'

# 更新数据表名称
python tools/dingtalk_notable.py update-sheet --base-id <id> --sheet-id <id> --name "新名称"

# 删除数据表
python tools/dingtalk_notable.py delete-sheet --base-id <id> --sheet-id <id>
```

### 字段操作

```bash
# 获取所有字段
python tools/dingtalk_notable.py list-fields --base-id <id> --sheet-id <id>
python tools/dingtalk_notable.py list-fields --base-id <id> --sheet-id <id> --format markdown

# 创建字段
python tools/dingtalk_notable.py create-field --base-id <id> --sheet-id <id> --name "优先级" --type singleSelect --property '{"choices":[{"name":"高"},{"name":"中"},{"name":"低"}]}'
python tools/dingtalk_notable.py create-field --base-id <id> --sheet-id <id> --name "金额" --type currency --property '{"currencyType":"CNY","formatter":"FLOAT_2"}'

# 更新字段
python tools/dingtalk_notable.py update-field --base-id <id> --sheet-id <id> --field-id <id> --name "新字段名"
python tools/dingtalk_notable.py update-field --base-id <id> --sheet-id <id> --field-id <id> --property '{"choices":[{"name":"P0"},{"name":"P1"},{"name":"P2"}]}'

# 删除字段
python tools/dingtalk_notable.py delete-field --base-id <id> --sheet-id <id> --field-id <id>
```

### 记录操作

```bash
# 新增记录（最多 100 条）
python tools/dingtalk_notable.py create-records --base-id <id> --sheet-id <id> --records '[{"fields":{"标题":"任务A","数量":10}},{"fields":{"标题":"任务B","数量":20}}]'
python tools/dingtalk_notable.py create-records --base-id <id> --sheet-id <id> --records-file /tmp/records.json

# 获取单条记录
python tools/dingtalk_notable.py get-record --base-id <id> --sheet-id <id> --record-id <id>

# 列出多行记录（分页）
python tools/dingtalk_notable.py list-records --base-id <id> --sheet-id <id>
python tools/dingtalk_notable.py list-records --base-id <id> --sheet-id <id> --max-results 50
python tools/dingtalk_notable.py list-records --base-id <id> --sheet-id <id> --max-results 50 --next-token <token> --format markdown

# 更新多行记录（需带 record id）
python tools/dingtalk_notable.py update-records --base-id <id> --sheet-id <id> --records '[{"id":"recXXX","fields":{"标题":"已更新","数量":99}}]'

# 删除多行记录
python tools/dingtalk_notable.py delete-records --base-id <id> --sheet-id <id> --record-ids recId1 recId2 recId3
```

### 附件上传

附件字段写入分两步：先上传文件获取资源引用，再将引用写入记录。

```bash
# 第一步：上传文件
python tools/dingtalk_notable.py upload-resource --base-id <id> --file /path/to/image.jpg

# 返回值示例：
# {"filename":"image.jpg","size":2048,"type":"image/jpeg","url":"/core/api/resources/img/xxx","resourceId":"uuid"}

# 第二步：将返回的信息写入附件字段
python tools/dingtalk_notable.py create-records --base-id <id> --sheet-id <id> --records '[{"fields":{"名称":"带附件","附件":[{"filename":"image.jpg","size":2048,"type":"image/jpeg","url":"/core/api/resources/img/xxx","resourceId":"uuid"}]}}]'
```

### 推荐操作流程

```
1. list-sheets → 了解数据表结构
2. list-fields → 了解字段定义（类型、属性）
3. list-records → 读取现有数据，确认字段名和值格式
4. 根据需求执行 create-records / update-records / delete-records
```

### 字段类型与属性

| type | 说明 | property（创建时传入） |
|------|------|------|
| `text` | 文本 | 无 |
| `number` | 数字 | `{"formatter":"FLOAT_2"}` 可选值: INT/FLOAT_1~4/THOUSAND/THOUSAND_FLOAT/PERCENT/PERCENT_FLOAT |
| `currency` | 货币 | `{"currencyType":"CNY","formatter":"FLOAT_2"}` currencyType: CNY/HKD/USD/EUR/GBP/MOP/VND/JPY/KRW/AED/AUD/BRL/CAD/CHF/INR/IDR/MXN/MYR/PHP/PLN/RUB/SGD/THB/TRY/TWD |
| `singleSelect` | 单选 | `{"choices":[{"name":"选项一"},{"name":"选项二"}]}` |
| `multipleSelect` | 多选 | 同 singleSelect |
| `date` | 日期 | `{"formatter":"YYYY-MM-DD HH:mm"}` 可选: YYYY-MM-DD/YYYY/MM/DD 等 |
| `user` | 人员 | `{"multiple":true}` 默认 true |
| `department` | 部门 | `{"multiple":true}` 默认 true |
| `checkbox` | 复选框 | 无 |
| `url` | 链接 | 无 |
| `attachment` | 附件 | 无 |
| `unidirectionalLink` | 单向关联 | `{"multiple":true,"linkedSheetId":"目标数据表ID"}` |
| `bidirectionalLink` | 双向关联 | `{"multiple":true,"linkedSheetId":"目标数据表ID"}` |

只读字段（API 自动维护，不可写入）：formula、creator、lastModifier、createdTime、lastModifiedTime

### 记录值格式

写入记录时 `fields` 用**字段名称**（不是 fieldId）作为 key。

| type | 写入值 | 读取返回值 |
|------|------|------|
| `text` | `"文本"` | `"文本"` |
| `number` | `123` (数值或字符串 `"123.45"`) | `"123"` (字符串) |
| `currency` | `99.5` (数值或字符串 `"99.5"`) | `"99.5"` (字符串) |
| `singleSelect` | `"选项名"` | `{"id":"xxx","name":"选项名"}` |
| `multipleSelect` | `["选项A","选项B"]` | `[{"id":"x","name":"选项A"},{"id":"y","name":"选项B"}]` |
| `date` | `1688601600000` (毫秒时间戳) 或 `"2025-06-15 09:30"` (字符串) | `1688601600000` |
| `user` | `[{"unionId":"xxx"}]` | `[{"unionId":"xxx"}]` |
| `department` | `[{"deptId":"xxx"}]` | `[{"deptId":"xxx"}]` |
| `checkbox` | `true` / `false` | `true` / `false` |
| `url` | `{"text":"钉钉","link":"https://dingtalk.com"}` | 同左 |
| `attachment` | `[{"filename":"x.jpg","size":200,"type":"image/jpeg","url":"<resourceUrl>","resourceId":"<resourceId>"}]` | 同左（url 为下载链接） |
| `unidirectionalLink` | `{"linkedRecordIds":["recId1","recId2"]}` | 同左 |
| `bidirectionalLink` | `{"linkedRecordIds":["recId1","recId2"]}` | 同左 |

### 参数说明

| 参数 | 说明 |
|------|------|
| `--base-id` | 多维表格 ID（从钉钉 URL `/nodes/<id>` 提取） |
| `--sheet-id` | 数据表 ID（从 list-sheets 获取） |
| `--field-id` | 字段 ID（从 list-fields 获取） |
| `--record-id` | 记录 ID |
| `--name` | 名称（数据表/字段） |
| `--type` | 字段类型 |
| `--property` | 字段属性 JSON |
| `--fields` | 创建数据表时的初始字段定义 JSON |
| `--records` / `--records-file` | 记录数据 JSON |
| `--record-ids` | 要删除的记录 ID 列表 |
| `--max-results` | 分页每页条数 |
| `--next-token` | 分页令牌 |
| `--format` | 输出格式：json / markdown |

### 限制与注意事项

- 单次 create / update / delete 最多 100 条记录
- create_fields 单次最多 15 个字段
- `number` / `currency` 查询返回字符串而非数值
- 分页：`list-records` 返回 `hasMore` + `nextToken`，需循环请求直到 `hasMore=false`
- API 无法读取公式计算值和引用值，只返回原始数据
- 更新 `singleSelect` / `multipleSelect` 的 choices 时要传完整列表（不是追加）
- 主字段保护：每个 Sheet 第一列为固定主字段，不可删除，类型仅支持系统指定范围
- 不能删最后一个字段
- 双向关联：创建 `bidirectionalLink` 字段时不传 `linkedFieldId`，由平台自动配对生成
- 日期推荐统一使用毫秒时间戳，避免时区与格式问题
- 附件 url 有时效性：在线文档附件无时效限制，其它文件附件 url 会过期，生产环境需注意

## 智能填表 — 群内问卷/信息收集

钉钉「智能填表」基于多维表格，典型场景：群里发问卷收集报名、反馈等。TyClaw 通过 `dingtalk_notable.py` 设计字段结构。

### 输出规范

- 不要解释"智能填表是什么"或"我能帮你做什么"，直接进入引导流程
- 全程不要提"文档 ID"，只说"链接"
- 不要自行编造问卷题目和选项，所有内容由用户决定
- 向用户总结时只说你创建/修改/添加了哪些题目，不要提"清理默认字段"等内部操作细节

### 交互流程

**第一步：了解用途，引导用户创建表单并发送链接**

用户说"帮我做个表单/问卷"时，请用户提供：
- **问卷用途**（必需）：这个问卷是做什么的？（如活动报名、满意度调查、信息收集等）
- **具体题目和选项**（可选）：如果已经想好了可以一起告诉我，没想好也没关系，告诉我用途后我可以和你一起设计

同时引导用户创建空白表单：

⚠️ 请确保你使用的是**新版钉钉智能填表**（旧版无法操作）。

**手机端操作：**

1. 打开钉钉，点击底部「工作台」
2. 找到并点击「智能填表」→「新建表单」
3. 进入编辑页后，**不要点右上角的「发布并分享」**！先点左上角 **<** 退出
4. 退出后在新页面，点右上角 **⋮**（三个点）→ 分享 → 复制链接
5. 把链接发给我

**电脑端操作：**

1. 打开钉钉，点击左侧「工作台」
2. 找到并点击「智能填表」→「新建表单」（请确认是新版）
3. 进入编辑页后，**不用编辑**，点击上方「分享」→ 复制链接
4. 把链接发给我

**第二步：设计表单字段**

从用户发来的链接中提取 `base_id`（链接格式 `https://alidocs.dingtalk.com/i/nodes/<base_id>?...`，取 `/nodes/` 后的部分），然后：

1. `list-sheets` → 获取数据表 ID
2. `list-fields` → 查看默认字段
3. 逐个 `delete-field` 清理默认字段（主字段不可删除，用 `update-field` 改名改类型复用）
4. 根据用户需求 `create-field` 创建字段
5. 完成后总结改动（列出每个题目名称和类型），并提醒用户接下来需要自己操作的事项：
   - 进入表单，把需要的题目设为「必填」
   - 点击表单下方「设置」，按需调整提交规则（如匿名、截止时间等）
   - 确认无误后把表单分享出去即可
   - 所有人填写的数据在「填写记录」中查看，后续也可以让我帮你做数据分析

**第三步：后续修改**

用户要求调整已有表单字段时，每次改完总结变更内容，提醒检查新增题目是否需要设为必填。

### 备选方案：通过 AI 表格创建

如果用户不方便手动新建表单，可由 TyClaw 直接创建一个多维表格（`create-node --doc-type NOTABLE`），设计好字段后把链接发给用户。用户收到后点击页面中间 **+** 号 → 选择「表单视图」即可生成表单。后续编辑必填项、设置、分享步骤相同。
