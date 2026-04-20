---
name: 钉钉日志
description: 查看钉钉日志（日报/周报），支持个人查询和部门查看
triggers:
  - 日报
  - 周报
  - 日志
  - report
requires: []
default: false
---

# 钉钉日志（只读）

查看钉钉日志（日报/周报等）。工具脚本：

- `tools/dingtalk_report.py` — 9 个子命令（只读）

## 行为规则

### 写日报/周报意图 → 告知不支持

用户要求"帮我写日报"、"生成日报"、"填周报"等**写入类意图**时，直接回复：

> 日志功能目前仅支持查看，不支持写入。请在钉钉中手动填写日报/周报。

### 查询类意图 → 先发现权限范围再执行

收到日志查询需求时，**第一步**调用 `my-scope` 了解当前用户的权限范围：

```
python tools/dingtalk_report.py my-scope
```

返回示例：

- 仅可查自己：`{"self": {"userid": "xxx"}}`
- 可查部门：

```json
{
  "self": {"userid": "xxx"},
  "departments": ["恒星工作室"],
  "total_members": 50,
  "members": [
    {"userid": "a1", "name": "张三", "department": "营销一组"},
    {"userid": "a2", "name": "李四", "department": "营销二组"},
    ...
  ]
}
```

`members` 包含权限范围内的所有成员，每个成员带 `department`（所属叶子部门）。用户说"帮我查张三的日报"时，可直接从 `members` 中找到对应 `userid`。

根据 `my-scope` 的结果决定后续操作：

- **无 `departments` 字段**：只能查自己的日志，用 `list-reports --days N`
- **有 `departments`**：可用 `--department` 查看范围内所有成员的日志，也可用 `--userid <id>` 查看指定成员
- **用户提到子团队名**：从 `members` 的 `department` 字段筛选成员，用 `--userid` 逐个查询

### 模板名称与空白日志

部分成员使用「空白日志」模板提交日报/周报/月报，而非标准模板。查询时**必须同时包含空白日志模板**，否则会漏人：

```
# 查日报 → 同时查"日报"和"空白日志"
--template-name 日报 空白日志

# 查周报
--template-name 周报 空白日志

# 查月报
--template-name 月报 空白日志
```

`--template-name` 支持传多个值，工具会分别查询每个模板并合并去重。仅当用户**明确指定单一模板**时才传单个值。

## 安全约束

1. **纯只读**：工具不提供任何写入命令
2. **权限由代理控制**：超出 `my-scope` 范围的查询会被代理拒绝，无需自行校验

## API 能力边界

- 时间范围上限 **180 天**（`--days` 不要超过 180）
- 日志正文中 Markdown 标记会被 strip 为纯文本
- `--simple` 模式不含正文，适合概览（谁交了/谁没交）
- 工具自动翻页，无需手动管理分页

## 子命令参考

### my-scope — 查询当前用户的权限范围

```
python tools/dingtalk_report.py my-scope
```

无参数。返回当前用户可查询的部门列表、成员总数和成员明细（含所属叶子部门）。**每次会话首次查询日志前必须调用。**

### list-templates — 列出可用的日志模板

```
python tools/dingtalk_report.py list-templates
```

| 参数 | 必填 | 说明 |
|------|------|------|
| --userid | 否 | 员工 userId，不传返回所有模板 |
| --offset | 否 | 分页游标，默认 0 |
| --size | 否 | 每页大小，默认 50 |

### get-template — 获取模板详情

```
python tools/dingtalk_report.py get-template --name 日报
```

| 参数 | 必填 | 说明 |
|------|------|------|
| --name | 是 | 模板名称（如: 日报、周报） |
| --userid | 否 | 操作员工 userId（不传使用当前用户） |

### list-reports — 查询日志列表

```
python tools/dingtalk_report.py list-reports --days 7
python tools/dingtalk_report.py list-reports --template-name 周报 空白日志 --days 30
python tools/dingtalk_report.py list-reports --department --days 7
python tools/dingtalk_report.py list-reports --department --days 1 --member-limit 100 --member-offset 0
python tools/dingtalk_report.py list-reports --userid <id> --days 7
```

| 参数 | 必填 | 说明 |
|------|------|------|
| --userid | 否 | 员工 userId（权限范围内的成员，代理校验） |
| --department | 否 | 查询权限范围内所有成员的日志（需 my-scope 有 departments） |
| --member-limit | 否 | 与 --department 配合：每批最多查询的成员数（默认不限） |
| --member-offset | 否 | 与 --department 配合：跳过前 N 个成员（默认 0） |
| --template-name | 否 | 模板名称过滤（支持多个值，分别查询后合并去重） |
| --days | 否 | 查询最近天数，默认 7 |
| --start-time | 否 | 起始时间 ms 时间戳（覆盖 --days） |
| --end-time | 否 | 结束时间 ms 时间戳 |
| --simple | 否 | 只返回概要（不含正文和修改时间） |

### get-statistics — 获取日志统计数据

```
python tools/dingtalk_report.py get-statistics --report-id <id>
```

| 参数 | 必填 | 说明 |
|------|------|------|
| --report-id | 是 | 日志 ID |

### list-related-users — 获取日志相关人员列表

```
python tools/dingtalk_report.py list-related-users --report-id <id> --type 0
```

| 参数 | 必填 | 说明 |
|------|------|------|
| --report-id | 是 | 日志 ID |
| --type | 是 | 类型: 0=已读 1=评论 2=点赞 |
| --offset | 否 | 分页游标，默认 0 |
| --size | 否 | 每页大小，默认 100 |

### list-receivers — 获取日志接收人员列表

```
python tools/dingtalk_report.py list-receivers --report-id <id>
```

| 参数 | 必填 | 说明 |
|------|------|------|
| --report-id | 是 | 日志 ID |
| --offset | 否 | 分页游标，默认 0 |
| --size | 否 | 每页大小，默认 100 |

### get-comments — 获取日志评论详情

```
python tools/dingtalk_report.py get-comments --report-id <id>
```

| 参数 | 必填 | 说明 |
|------|------|------|
| --report-id | 是 | 日志 ID |
| --offset | 否 | 分页游标，默认 0 |
| --size | 否 | 每页大小，默认 20 |

### get-unread — 获取用户日志未读数

```
python tools/dingtalk_report.py get-unread
```

| 参数 | 必填 | 说明 |
|------|------|------|
| --userid | 否 | 员工 userId（不传使用当前用户） |

## 基本流程

1. `my-scope` → 了解权限范围和成员列表
2. 根据意图选择 `list-reports` 的参数组合（`--department` / `--userid` / `--simple` / `--days` 等）
3. 人数较多时用 `--member-limit` / `--member-offset` 分批，或 `--simple` 先看概览再深入
