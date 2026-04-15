---
name: GA数据查询
description: 通过 SQL 查询 GA 平台的事件、用户、设备等数据
triggers:
  - 查一下数据
  - 帮我查数据
  - 帮我跑个SQL
  - 执行SQL
  - GA查询
  - GA数据
  - 查一下事件
  - 设置GA
  - 设置GA凭证
  - GA账号
tool: tools/ga_query.py
default: false
credentials:
  key: ga
  display_name: GA
  setup: manual
  fields:
    - {name: username, label: GA APIKEY 用户名}
    - {name: password, label: GA APIKEY 密码, secret: true}
---
# GA SQL 查询

通过 MySQL 协议连接 GA 数据网关执行 SQL，查询事件、用户、设备、分群等数据。

## 凭证配置

使用前需先设置 GA 凭证。注意：GA 凭证与 GA 平台登录账号密码不同，需从 [Koda](https://analytics.tuyoo.com/hermes/dashboard/koda) 输入 `/ga-apikey-management` 创建 APIKEY 获取。

```bash
python3 tools/ga_query.py setup --username "user@tuyoogame.com" --password "your-ga-apikey-password"
```

清除凭证：

```bash
python3 tools/ga_query.py clear-credentials
```

当用户说「设置GA凭证」「GA账号」时，先提醒用户 GA 凭证与 GA 平台登录账号密码不同，需从 Koda 创建 APIKEY 获取。然后询问用户提供 username 和 password，调用 `setup`。

## 工具用法

`tools/ga_query.py`（从 TyClaw 项目根目录运行）

### 常用命令

```bash
# 执行 SQL 查询（markdown 表格输出）
python3 tools/ga_query.py --sql "SELECT day, count(DISTINCT user_id) AS uv FROM table.event_20606 WHERE event_id='sdk_s_login_succ' AND day BETWEEN '2026-03-01' AND '2026-03-10' GROUP BY day ORDER BY day" --format markdown

# 执行 SQL 查询（JSON 输出）
python3 tools/ga_query.py --sql "SELECT ..."

# 列出当前用户可访问的项目
python3 tools/ga_query.py --list-projects

# 列出可用表（需 --project-id）
python3 tools/ga_query.py --list-tables --project-id 20606

# 探查项目 schema（表、维度、字段元信息）
python3 tools/ga_query.py --discover --project-id 20606

# 指定项目 ID
python3 tools/ga_query.py --sql "SELECT ..." --project-id 20249

# 控制输出长度
python3 tools/ga_query.py --sql "SELECT ..." --format markdown --max-length 5000
```

### 参数速查

| 参数 | 说明 | 必须 |
|------|------|------|
| `--sql` | SQL 查询语句 | 查询时必须 |
| `--list-projects` | 列出当前用户可访问的项目 ID | 独立使用 |
| `--list-tables` | 列出可用表 | 独立使用，需 `--project-id` |
| `--discover` | 探查项目 schema（表+维度+字段） | 独立使用，需 `--project-id` |
| `--project-id` | GA 项目 ID | `--list-tables`/`--discover` 必须 |
| `--format` | 输出格式：json（默认）或 markdown | 可选 |
| `--max-length` | 截断输出字符数（0 = 不限） | 可选 |

### 输出格式

JSON 模式返回完整响应，关键字段：
- `ifSuccess`: 查询是否成功
- `header`: 列名列表
- `result`: 结果数组（每行一个 dict）
- `error`: 错误信息（失败时）

Markdown 模式直接输出表格。通过 MySQL 协议直连，无需 token 管理。状态信息输出到 stderr，查询结果输出到 stdout。

## SQL 平台规则

### 元数据命令

表和字段信息通过 GA 内置元数据命令动态获取，可直接用 `--sql` 执行：

| 命令 | 说明 |
|------|------|
| `SHOW PROJECTS` | 列出当前用户可访问的所有项目 ID |
| `SHOW TABLES IN <pid>` | 列出项目所有表 |
| `SHOW DIMS IN <pid>` | 列出维度表（用户/设备/角色/服务器等） |
| `SHOW EVENTS IN <pid>` | 列出所有事件 ID |
| `SHOW EVENT_PROPERTIES IN <pid>` | 事件表字段（含类型、中文名、类型转换提示） |
| `SHOW PROPERTIES IN <pid> WHERE dimension = '<dim>'` | 维度表字段（dim 来自 SHOW DIMS 的 alias） |
| `DESCRIBE <table_name>` | 底层物理列（ClickHouse 类型） |

也可通过 `--list-tables` 和 `--discover` 快捷调用。

**重要**：SQL 中表名必须带项目 ID 后缀，如 `table.event_20606`。

**项目 ID 获取规则**：
- `--project-id` 必须由调用方传入
- 如果你不知道项目名对应的 project_id，**直接问用户**
- 禁止读取 config/ 目录或 _personal/ 目录来查找项目 ID

### 基础约束

- **时间范围**：单次 `BETWEEN` 最多 99 天。超过需用 OR 按季度拼接
- **字段类型**：大部分字段是 string，数值运算需 `cast`，如 `cast(amount as bigint)`
- **超时**：同步接口 5 分钟超时，控制查询范围
- **事件表查询**：必须用 `event_id` 和 `day` 做筛选，避免全表扫描

### 支持的 SQL 特性

| 特性 | 示例 |
|------|------|
| CTE（WITH 子句） | `WITH t0 AS (...) SELECT ...` |
| 窗口函数 | `ROW_NUMBER() OVER (PARTITION BY x ORDER BY y)` |
| REGEXP | `WHERE field regexp 'pattern'` |
| CASE WHEN | `CASE WHEN x > 1 THEN 'A' ELSE 'B' END` |
| DATEDIFF | `DATEDIFF(day1, day2)` |
| CAST | `CAST(field AS bigint/float)` |
| COLLECT_SET | `COLLECT_SET(CAST(field AS STRING))` |
| arraySort | `arraySort(array)` |

## 安全约束

1. 禁止执行 DDL 语句（CREATE/DROP/ALTER/TRUNCATE）
2. 仅允许 SELECT 查询
3. 完成后直接输出结果
