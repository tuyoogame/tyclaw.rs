---
name: TD投放查询
description: 查询 TD 平台投放数据，包括消耗、ROI、LTV、留存、新增等报表指标，支持优化目标和素材维度
triggers:
  - 投放数据
  - 查报表
  - 看报表
  - 消耗
  - 花费
  - ROI
  - LTV
  - 新增
  - 付费
  - 留存
  - 分小时
  - 优化目标
  - 素材
  - 设置TD
  - 设置TD凭证
  - TD账号
tool: tools/td_query.py
default: false
credentials:
  key: td
  display_name: TD
  setup: token
  fields:
    - {name: token, label: Authorization Token, secret: true}
---
# TD 投放数据查询

> **⚠️ 维护公告：TD 系统近期进行了安全升级，相关 API 接口调用能力受到影响，目前暂无法从服务端访问，仅支持网页端使用。我们正在与相关团队协同推进恢复方案，具体进展会及时同步。**
>
> **收到 TD 相关查询时，请直接将上述维护信息告知用户，不要调用 td_query.py 工具。**

通过自然语言描述查询需求，自动构造请求参数并调用 Ad Manager 报表 API，查询投放消耗、ROI、LTV、留存等数据。

> 本文件位于 `skills/td-query/`，下文中的相对路径均基于此目录。

使用前先读取 [references/endpoints.md](references/endpoints.md) 获取端点定义，读取 [references/columns.md](references/columns.md) 获取可用指标字段，读取 [references/filters.md](references/filters.md) 获取筛选参数的 ID 映射。

## 凭证配置

```bash
python3 tools/td_query.py setup --token "jwt-token"
```

清除凭证：

```bash
python3 tools/td_query.py clear-credentials
```

当用户说「设置TD凭证」「TD账号」时，告知获取 Token 的步骤：
1. 浏览器打开 TD 平台并登录
2. 按 F12 打开开发者工具
3. 切换到 Application 标签页
4. 左侧找到 Local Storage，点击展开
5. 找到 Authorization，复制它的值

等用户发来 token 后调用 `setup`。

## 工具用法

`tools/td_query.py`（从 TyClaw 项目根目录运行）

### 常用命令

```bash
# 常规报表（markdown 表格输出）
python3 tools/td_query.py --report --body '{"page":1,"page_size":20,"start_date":"2026-02-28","end_date":"2026-02-28","order_by":"cost","order_type":"desc","dimension":5,"income_type":1,"column_list":["cost","new_user","roi1"]}' --format markdown

# 分小时报表
python3 tools/td_query.py --hourly --body '{"date":"2026-03-02","project_id":2,"platform":1}' --format markdown

# 下载报表 — 渠道聚合（按渠道+应用）
python3 tools/td_query.py --download --report-type channel --body '{"dimension_list":[5,3],"start_date":"2026-03-19","end_date":"2026-03-19","order_by":"cost","order_type":"desc","column_list":["cost","new_user","roi1"]}'

# 下载报表 — 渠道明细（按渠道+优化目标）
python3 tools/td_query.py --download --report-type channelDetail --body '{"dimension":1,"dimension_items":[3,16],"start_date":"2026-03-19","end_date":"2026-03-19","order_by":"cost","order_type":"desc","page":1,"page_size":100,"is_download":true,"is_detail":false,"column_list":["cost","new_user","roi1","optimization_goal"],"project":["6997"]}'

# JSON 原始输出
python3 tools/td_query.py --report --body '...' --format json

# 控制输出长度
python3 tools/td_query.py --report --body '...' --format markdown --max-length 5000
```

### 参数速查

| 参数 | 说明 | 必须 |
|------|------|------|
| `--report` | 常规报表模式 | 三选一 |
| `--hourly` | 分小时报表模式 | 三选一 |
| `--download` | 下载报表模式 | 三选一 |
| `--report-type` | 下载报表类型：channel / channelDetail | `--download` 时必须 |
| `--body` | JSON 请求体 | 是 |
| `--format` | 输出格式：markdown（默认）或 json | 可选 |
| `--max-length` | 截断输出字符数（0 = 不限） | 可选 |
| `--max-rows` | 下载模式最大展示行数（默认 50） | 可选 |
| `--grep` | 按关键词过滤行（匹配任意列，大小写不敏感） | 可选 |

## 端点选择规则

| 用户意图 | 模式 | 说明 |
|---------|------|------|
| 常规报表（按天/渠道/账户等） | `--report` | 支持多维度、分页、排序、自选指标 |
| 分小时数据（小时级消耗/趋势） | `--hourly` | 单日每小时明细，固定返回字段 |
| 优化目标/素材类型/素材查询 | `--download --report-type channelDetail`（`is_detail: false`） | 用 dimension_items 聚合，支持扩展维度 |
| 素材明细（逐条素材+预览链接） | `--download --report-type channelDetail`（`is_detail: false` + dimension_items 含 `19`） | 按素材md5聚合，自动返回素材名称/预览链接/封面链接 |
| 渠道报表下载（多维度聚合） | `--download --report-type channel` | 同常规报表维度，以下载方式获取 |

**判断关键词：**
- "分小时""小时级""按小时看""每小时""小时趋势""小时消耗" → `--hourly`
- "优化目标""按优化目标""素材类型""投放版位" → `--download --report-type channelDetail`
- "素材""查素材""看素材""素材数据""投放素材" → `--download --report-type channelDetail` + `dimension_items` 包含 `19`（素材md5）

## 常规报表参数构造规则

### 日期范围 (start_date / end_date)

格式 `YYYY-MM-DD`，根据自然语言解析：

| 用户表述 | start_date | end_date |
|---------|-----------|---------|
| 今天 | 当天日期 | 当天日期 |
| 昨天 | 昨天日期 | 昨天日期 |
| 最近 N 天 | 当天 - (N-1) 天 | 当天 |
| 上周 | 上周一 | 上周日 |
| 本月 | 本月1日 | 当天 |
| 具体日期 2026-02-20 | 2026-02-20 | 2026-02-20 |
| 日期区间 2/20-2/28 | 2026-02-20 | 2026-02-28 |

未指定日期时默认为**今天**。

### 维度 (dimension)

| 值 | 含义 | 用户表述示例 |
|----|------|-------------|
| 1 | 日 | "按天看"、"每日趋势" |
| 2 | 月 | "按月看"、"月度汇总" |
| 3 | 投放应用 | "按应用看"、"各应用" |
| 4 | 子平台 | "按子平台" |
| 5 | 渠道 | "按渠道看"、"各渠道" |
| 6 | 计划名 | "按计划名" |
| 7 | 计划ID | "按计划ID" |
| 8 | 账户名称 | "按账户看"、"各账户" |
| 9 | 账户ID | "按账户ID" |
| 10 | 广告名称 | "按广告看"、"各广告" |
| 11 | 广告ID | "按广告ID" |
| 12 | 营销工作室 | "按工作室" |
| 13 | 优化师 | "按优化师" |
| 14 | 二级渠道 | "按二级渠道" |
| 15 | 推广类型 | "按推广类型" |
| 16 | 账户备注 | "按账户备注" |
| 17 | 代理商 | "按代理商" |
| 18 | 营销组 | "按营销组" |
| 19 | 投放策略 | "按投放策略" |
| 20 | 开户主体 | "按开户主体" |
| 21 | 周 | "按周看"、"每周汇总" |

用户未指定维度时默认 `dimension: 5`（渠道）。

### 收入类型 (income_type)

| 值 | 含义 | 用户表述示例 |
|----|------|-------------|
| 1 | 净收入 | "净收入"、"扣量后" |
| 2 | 账面收入 | "账面收入"、"账面" |

用户未指定时默认 `income_type: 2`（账面收入）。

### 指标字段 (column_list)

根据用户关注的指标选择字段子集。完整字段列表见 [references/columns.md](references/columns.md)。

常用分组：
- **投放数据**: `cost`, `show`, `click`, `convert`, `active`, `ctr`, `cvr`
- **成本效率**: `convert_cost`, `active_cost`, `cost_by_thousand_show`, `new_user_cost`, `new_paid_user_cost`
- **用户**: `new_user`, `new_paid_user`, `new_paid_rate`, `new_user_ad_trace`, `advertiser_dau`
- **付费**: `pay1`~`pay360`, `total_pay`, `pay3_1`, `pay7_1`, `arppu_1`, `advertiser_recharge`, `income`
- **LTV**: `ltv1`~`ltv360`, `total_ltv`
- **ROI**: `roi1`~`roi360`, `total_roi`
- **留存率**: `stay2`~`stay180`；留存人数: `stay_num2`~`stay_num180`
- **付费留存**: `pay2_stay_rate`, `pay3_stay_rate`, `pay7_stay_rate`
- **活跃天数**: `times3`~`times7`

payN / ltvN / roiN / stayN 支持的天数节点: 1-15, 30, 45, 60, 90, 120, 150, 180, 210, 240, 270, 300, 360（roi 从 roi1 到 roi7 连续，之后跳到 roi15）。

用户未明确指定指标时，使用投放概览默认组合：
```json
["cost","show","click","ctr","cvr","new_user","new_paid_user","pay1","roi1","roi7","total_roi","stay2"]
```

### 排序 (order_by / order_type)

- `order_by`: 排序字段名，须为 `column_list` 中的字段
- `order_type`: `asc` 升序 / `desc` 降序

| 用户表述 | order_by | order_type |
|---------|---------|-----------|
| 按花费排序 / 花费最多 | cost | desc |
| 按 ROI 排序 | roi1 | desc |
| 新增最少 | new_user | asc |

未指定排序时默认 `order_by: "cost"`, `order_type: "desc"`。

### 分页 (page / page_size)

- 默认 `page: 1`, `page_size: 20`
- 用户说"前 50 条" → `page_size: 50`
- 用户说"第 2 页" → `page: 2`

### 筛选条件（可选参数）

用户提到筛选条件时才加入对应字段，未提及则**不传**。

| 参数 | 类型 | 用户表述示例 |
|------|------|-------------|
| project | string[] | "欢乐钓鱼大师" → `["9420"]`，ID 映射见 filters.md |
| platform | string[] | "头条" → `["1"]`，"快手" → `["2"]`，ID 映射见 filters.md |
| sub_channel | string | "子渠道4" → `"4"` |
| device_os | number | "iOS" / "安卓" → 对应数值 |
| studio | string[] | "工作室1" → `["1"]` |
| spread_type | string | "推广类型1" → `"1"` |
| account_id_list | string[] | "账户 1233323" → `["1233323"]` |
| strategy | number | "自动投放" → `2`，"手动投放" → `1` |

筛选参数的值为 ID，用户使用名称时查 [references/filters.md](references/filters.md) 转换为 ID。无法匹配时提示确认。

## 下载模式参数构造规则

### 何时使用下载模式

当用户查询涉及以下维度时，**必须使用** `--download --report-type channelDetail`（在线 `--report` 不支持）：
- 优化目标、素材类型、投放版位、素材明细
- 需要 `optimization_goal`、`deep_optimization_goal` 等字段

常规维度（渠道、应用、账户等）如果不涉及上述维度，优先使用 `--report`（响应更快、支持分页）。

### 渠道报表下载 (channel)

`--download --report-type channel --body '{...}'`

body 参数与 `--report` 基本相同，但维度改为 `dimension_list`（数组，支持多维度聚合）：

```json
{
  "dimension_list": [5, 3],
  "start_date": "2026-03-19",
  "end_date": "2026-03-19",
  "order_by": "cost",
  "order_type": "desc",
  "column_list": ["cost", "new_user", "roi1"],
  "project": ["6997"]
}
```

`dimension_list` 枚举值与 `--report` 的 `dimension` 相同（1=日, 5=渠道, 3=投放应用...）。

### 渠道明细下载 (channelDetail)

`--download --report-type channelDetail --body '{...}'`

```json
{
  "dimension": 1,
  "dimension_items": [3, 16],
  "start_date": "2026-03-19",
  "end_date": "2026-03-19",
  "order_by": "cost",
  "order_type": "desc",
  "page": 1,
  "page_size": 100,
  "is_download": true,
  "is_detail": false,
  "column_list": ["cost", "new_user", "roi1"],
  "project": ["6997"]
}
```

**固定参数**：`"is_download": true`

**`is_detail`**：**始终设为 `false`**。素材查询通过 `dimension_items` 包含 `19`（素材md5）实现，API 会自动返回素材名称、预览链接、封面链接等字段，无需 `is_detail: true`。

> **禁止使用 `is_detail: true`**：会返回数万条未聚合的广告×素材笛卡尔积（同一素材在不同广告/计划/账户下各出一行），数据量巨大（18MB+）且忽略分页参数。`is_detail: false` + 素材md5维度会自动按素材去重聚合，数据量减少一个数量级，响应也更快（直接返回 Excel，无需异步任务）。

**`dimension_items`** 枚举（dime_pm_*，与 --report 的 dimension 编号不同！）：

| 值 | 含义 | 用户表述 |
|----|------|---------|
| 1 | 日 | "按天" |
| 3 | 渠道 | "按渠道" |
| 7 | 投放应用 | "按应用" |
| 9 | 子平台 | "按子平台" |
| 16 | 优化目标 | "按优化目标" |
| 18 | 素材类型 | "按素材类型" |
| 19 | 素材md5 | "按素材""查素材"——自动返回素材名称/预览链接/封面链接 |
| 5 | 营销工作室 | "按工作室" |
| 6 | 优化师 | "按优化师" |
| 8 | 推广类型 | "按推广类型" |
| 10 | 账户ID | "按账户" |
| 12 | 计划ID | "按计划" |
| 14 | 广告ID | "按广告" |
| 22 | 营销组 | "按营销组" |
| 23 | 投放策略 | "按投放策略" |
| 24 | 开户主体 | "按开户主体" |
| 27 | 周 | "按周" |

完整枚举见 [references/endpoints.md](references/endpoints.md)。

**channelDetail 特有的 column_list 字段**（在线 `--report` 不支持）：
- `optimization_goal` — 优化目标名称
- `deep_optimization_goal` — 深度优化目标
- `cpc`, `cpm` — 单击成本、千展成本

**素材信息字段**：使用 `dimension_items` 包含 `19`（素材md5）时，API 会在返回的 Excel 中自动附带以下字段（无需加入 column_list）：
- 视频md5值、素材名称、预览链接、封面链接

用户未明确指定指标时，默认：
```json
["cost", "new_user", "roi1", "roi7", "total_roi"]
```

### 筛选参数

与 `--report` 共用 filters.md 中的 ID 映射，参数名和类型也基本相同。

## 分小时端点参数构造规则

### 日期 (date)

仅支持单日，格式 `YYYY-MM-DD`。用户说"今天"就用当天日期，说"昨天"用昨天。

### 筛选参数（可选）

与常规报表共用 filters.md 中的 ID 映射，但**参数类型不同**：

| 参数 | 类型 | 说明 | 对比常规报表 |
|------|------|------|-------------|
| project_id | number | 项目 ID | 常规为 `project: string[]` |
| platform | number | 平台 ID | 常规为 `platform: string[]` |
| sub_channel | number | 子渠道 ID | 常规为 `sub_channel: string` |
| device_os | number | 设备系统 | 相同 |
| studio_id | number | 工作室 ID | 常规为 `studio: string[]` |
| spread_type | string | 推广类型 | 相同 |
| account_id | string | 单个账户 ID | 常规为 `account_id_list: string[]` |

用户未提及的筛选条件**不传**。

## 自然语言映射示例

**示例 1**: "查一下今天的投放数据"

```bash
python3 tools/td_query.py --report --body '{"page":1,"page_size":20,"start_date":"2026-03-13","end_date":"2026-03-13","order_by":"cost","order_type":"desc","dimension":5,"income_type":2,"column_list":["cost","new_user","new_paid_user","pay1","roi1","roi7","total_roi","stay2"]}' --format markdown
```

**示例 2**: "最近 7 天花费最多的前 10 条，只看花费和 ROI"

```bash
python3 tools/td_query.py --report --body '{"page":1,"page_size":10,"start_date":"2026-03-07","end_date":"2026-03-13","order_by":"cost","order_type":"desc","dimension":5,"income_type":2,"column_list":["cost","roi1","roi7","total_roi"]}' --format markdown
```

**示例 3**: "三国冰河时代头条和广点通，2月28号的消耗和ROI"

```bash
python3 tools/td_query.py --report --body '{"page":1,"page_size":20,"start_date":"2026-02-28","end_date":"2026-02-28","order_by":"cost","order_type":"desc","dimension":5,"income_type":2,"column_list":["cost","new_user","roi1","pay1","new_paid_user"],"project":["6997"],"platform":["1","3"]}' --format markdown
```

**示例 4**: "今天3D捕鱼头条信息流的分小时数据"

```bash
python3 tools/td_query.py --hourly --body '{"date":"2026-03-13","project_id":2,"platform":1,"sub_channel":4}' --format markdown
```

**示例 5**: "看一下今天欢乐钓鱼大师安卓端按小时的消耗"

```bash
python3 tools/td_query.py --hourly --body '{"date":"2026-03-13","project_id":9420,"device_os":1}' --format markdown
```

**示例 6**: "三国冰河今天按渠道和优化目标看消耗和ROI"

```bash
python3 tools/td_query.py --download --report-type channelDetail --body '{"dimension":1,"dimension_items":[3,16],"start_date":"2026-03-20","end_date":"2026-03-20","order_by":"cost","order_type":"desc","page":1,"page_size":100,"is_download":true,"is_detail":false,"column_list":["cost","new_user","roi1"],"project":["6997"]}' --format markdown
```

**示例 7**: "今天头条按素材类型看消耗"

```bash
python3 tools/td_query.py --download --report-type channelDetail --body '{"dimension":1,"dimension_items":[3,18],"start_date":"2026-03-20","end_date":"2026-03-20","order_by":"cost","order_type":"desc","page":1,"page_size":100,"is_download":true,"is_detail":false,"column_list":["cost","new_user"],"platform":["1"]}' --format markdown
```

**示例 8**: "查一下三国冰河今天头条消耗最高的素材"

```bash
python3 tools/td_query.py --download --report-type channelDetail --body '{"dimension":1,"dimension_items":[3,19],"start_date":"2026-03-21","end_date":"2026-03-21","order_by":"cost","order_type":"desc","page":1,"page_size":20,"is_download":true,"is_detail":false,"column_list":["cost","new_user","roi1"],"project":["6997"],"platform":["1"]}' --format markdown --max-rows 10
```

> dimension_items 包含 `19`（素材md5），API 自动返回素材名称、预览链接、封面链接列，无需在 column_list 中指定。

**示例 9**: "查一下三国冰河标题带'冰雪三国真上头'的素材数据"

```bash
python3 tools/td_query.py --download --report-type channelDetail --body '{"dimension":1,"dimension_items":[19],"start_date":"2026-03-20","end_date":"2026-03-20","order_by":"cost","order_type":"desc","page":1,"page_size":100,"is_download":true,"is_detail":false,"column_list":["cost","new_user","roi1","roi7","total_roi"],"project":["6997"]}' --format markdown --max-rows 10 --grep "冰雪三国真上头"
```

> **按素材名称搜索**：TD API 不支持服务端按名称过滤。使用 `--grep` 在下载后的 Excel 中过滤，只构建匹配行，避免全量解析。务必通过 `project` 筛选缩小数据范围。

## 歧义处理

- 用户提到本 API 不支持的指标 → 说明不支持并列出可用指标
- 日期表述不清 → 默认今天，并告知用户使用的日期范围
- 排序字段不在 column_list 中 → 自动加入 column_list
- 用户用名称描述筛选条件（如"腾讯渠道"）但无法确定 ID → 提示用户确认对应 ID
- 用户未提及筛选条件 → 不传筛选参数，查询全量数据

## 安全约束

1. 此工具仅做只读查询，无写入操作
2. 完成后直接输出结果
