---
name: 钉钉待办
description: 管理钉钉待办任务，包括创建/更新/查询待办、标记完成状态、指派给他人
triggers:
  - 待办
  - 待办任务
  - 创建待办
  - 更新待办
  - 标记完成
  - todo
  - task
requires: []
default: false
---

# 钉钉待办

管理钉钉待办任务。工具脚本：

- `tools/dingtalk_todo.py` — 4 个子命令

授权管理：`tools/dingtalk_auth.py`（复用钉钉云文档的 OAuth 流程）

## 安全约束（必须遵守）

1. **创建待办时确认关键信息**：标题、截止时间、指派对象，向用户确认后再执行
2. **标记完成/未完成前确认**：告知用户操作的待办标题和目标状态
3. **指派给他人时解析身份**：通过 `/api/resolve-user` 解析人名 → staff_id，重名时展示部门让用户选择

## 时间格式规范

- 截止时间：毫秒级 Unix 时间戳，或 ISO-8601 格式 `2026-04-10T18:00:00+08:00`
- 工具内部自动转换 ISO-8601 → 毫秒时间戳
- 用户说"今天下午6点"时，需根据当前日期换算为具体时间

## 参与者解析流程

用户提到人名时，按以下流程解析：

1. **批量解析**（推荐）：多个人名用一次 `/api/resolve-users` 全部解析完。单个人名也可以用 `/api/resolve-user`

```bash
# 批量解析（POST，body 为 JSON）
python3 -c "
import urllib.request, json, os
url = os.environ['_TYCLAW_DT_PROXY_URL'].replace('/api/dingtalk-proxy', '/api/resolve-users')
token = os.environ['_TYCLAW_DT_PROXY_TOKEN']
data = json.dumps({'token': token, 'names': ['张三', '李四']}).encode()
req = urllib.request.Request(url, data=data, headers={'Content-Type': 'application/json'})
resp = urllib.request.urlopen(req)
print(resp.read().decode())
"
# 返回: {"results": {"张三": {"staff_id": "xxx", "name": "张三"}, "李四": {"staff_id": "yyy", "name": "李四"}}}
# 重名时某个 name 返回 {"candidates": [...]}, 展示部门让用户选择
```

2. 将 staff_id 传入 `--executors` 或 `--participants` 参数，工具内部用 `__staff:xxx__` 占位符，代理自动替换为 unionId
3. 如果代理返回 `STAFF_UNIONID_NOT_BOUND` 错误，说明目标用户未完成钉钉授权。直接将错误 message 转述给用户（已包含姓名和操作指引）

## 子命令参考

### create-task — 创建待办任务

```
python tools/dingtalk_todo.py create-task \
  --subject "修复登录页 bug" \
  --description "iOS 端偶现闪退，优先排查" \
  --due-time 2026-04-11T18:00:00+08:00 \
  --priority 30 \
  --executors 621942314220944463 \
  --participants 011533646711841213 \
  --notify-ding
```

| 参数 | 必填 | 说明 |
|------|------|------|
| --subject | 是 | 待办标题（最大 1024 字符） |
| --description | 否 | 待办备注（最大 4096 字符） |
| --due-time | 否 | 截止时间（毫秒时间戳或 ISO-8601） |
| --priority | 否 | 优先级: 10=低 / 20=普通 / 30=紧急 / 40=非常紧急 |
| --executors | 否 | 执行者 staff_id 列表（空格分隔） |
| --participants | 否 | 参与者 staff_id 列表（空格分隔） |
| --creator | 否 | 创建者 staff_id（不传则使用当前用户） |
| --source-id | 否 | 业务侧唯一标识（幂等） |
| --source-title | 否 | 来源标题 |
| --detail-app-url | 否 | 移动端跳转链接 |
| --detail-pc-url | 否 | PC 端跳转链接 |
| --todo-type | 否 | 业务类型: TODO(待办) / READ(待阅) |
| --executor-only | 否 | 仅执行者可见 |
| --notify-ding | 否 | 发送 ding 通知 |

### update-task — 更新待办任务

```
python tools/dingtalk_todo.py update-task \
  --task-id <taskId> \
  --subject "新标题" \
  --done true
```

| 参数 | 必填 | 说明 |
|------|------|------|
| --task-id | 是 | 待办任务 ID |
| --subject | 否 | 新标题 |
| --description | 否 | 新备注 |
| --due-time | 否 | 新截止时间 |
| --done | 否 | 完成状态: true/false |
| --priority | 否 | 优先级 |
| --executors | 否 | 新执行者 staff_id 列表 |
| --participants | 否 | 新参与者 staff_id 列表 |

### update-executor-status — 更新执行者完成状态

```
python tools/dingtalk_todo.py update-executor-status \
  --task-id <taskId> \
  --executors 621942314220944463 \
  --is-done true
```

| 参数 | 必填 | 说明 |
|------|------|------|
| --task-id | 是 | 待办任务 ID |
| --executors | 是 | 执行者 staff_id 列表 |
| --is-done | 是 | 完成状态: true/false |

### query-tasks — 查询企业下用户待办列表

```
python tools/dingtalk_todo.py query-tasks --is-done false --role-types executor creator
```

| 参数 | 必填 | 说明 |
|------|------|------|
| --next-token | 否 | 分页游标 |
| --is-done | 否 | 完成状态筛选: true/false |
| --todo-type | 否 | 业务类型筛选: TODO/READ |
| --role-types | 否 | 角色筛选: executor / creator / participant（多选，外层 OR） |

## 典型场景

### 创建待办并指派给他人

1. 通过 `/api/resolve-user` 解析人名 → staff_id
2. `create-task --subject "..." --executors <staff_id> --due-time ...`

### 查看自己的未完成待办

1. `query-tasks --is-done false`
2. 将结果格式化为表格展示

### 标记待办完成

1. 先 `query-tasks` 找到目标待办的 taskId
2. `update-executor-status --task-id <id> --executors <自己的 staff_id> --is-done true`
