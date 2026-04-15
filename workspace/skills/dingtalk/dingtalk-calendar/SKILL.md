---
name: 钉钉日程
description: 管理钉钉日程，包括创建/查询/修改日程、管理参与者、查看忙闲状态、预定会议室
triggers:
  - 日程
  - 日历
  - 会议
  - 会议室
  - 忙闲
  - 约会
  - 开会
  - 预约
  - 订会议室
  - calendar
  - schedule
requires: []
default: false
---

# 钉钉日程

管理钉钉日程、参与者、忙闲查询和会议室预定。工具脚本：

- `tools/dingtalk_calendar.py` — 15 个子命令

授权管理：`tools/dingtalk_auth.py`（复用钉钉文档的 OAuth 流程）

## 安全约束（必须遵守）

1. **删除日程前必须确认身份和后果**：
   - 先调 `get-event` 确认当前用户是组织者（organizer.self=true）还是参与者
   - **组织者删除 = 取消会议**：日程从所有参与者日历中删除，所有人收到取消通知，**不可撤销**
   - **参与者删除 = 退出会议**：仅从自己日历移除，不影响其他人
   - 必须向用户明确说明是"取消会议（你是组织者）"还是"退出会议（你是参与者）"，获得确认后再执行
   - 用户说"取消会议"时，如果不是组织者，要告知只能退出不能取消
2. **修改日程仅组织者可操作**：`update-event` 前先确认当前用户是组织者，非组织者调用会返回错误
3. **添加/移除参与者仅组织者可操作**：`add-attendees` 和 `remove-attendees` 前先确认组织者身份，并展示人选（姓名+部门）让用户确认
4. **不能代其他用户操作**：所有操作以当前用户身份执行（path userId 由代理自动注入）
5. **参与者变更必须事后核验**：`add-attendees` / `remove-attendees` 执行后，立即调 `list-attendees` 获取最新参与者列表，对比操作前后的 displayName 确认实际变更与预期一致。发现异常（如添加了非预期人员）必须立即告知用户并回滚

## 会议室主动引导

创建日程时，如果用户提到"线下会议"/"面对面"/"当面"等明确的线下关键词，**主动询问是否需要预定会议室**（除非用户已明确不需要）。

**重要：先问会议室，再创建日程。** 确认需要会议室后，先走"创建带会议室的日程" Step 1-3 完成选房，最后在 Step 4 一次性 `create-event` + `book-room`。不要先 `create-event` 再问会议室。

## 时间格式规范

- 非全天日程：ISO-8601 格式 `2026-04-10T14:00:00+08:00`，时区固定 `Asia/Shanghai`
- 全天日程：日期格式 `yyyy-MM-dd`。工具会自动处理：
  - `--start` 传日期格式时自动识别为全天日程（无需显式 `--all-day`）
  - `--end` 未传时自动设为 start+1 天；end == start 时自动 +1 天
- 用户说"明天下午3点"时，你需要根据当前日期换算为具体 ISO-8601 时间

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

2. 将 staff_id 传入 `--staff-ids` 参数，工具内部用 `__staff:xxx__` 占位符，代理自动替换为 unionId
3. 如果代理返回 `STAFF_UNIONID_NOT_BOUND` 错误，说明目标用户未完成钉钉授权。直接将错误 message 转述给用户（已包含姓名和操作指引）

## 子命令参考

### 日程基础操作

#### create-event — 创建日程

```
python tools/dingtalk_calendar.py create-event \
  --summary "周会" \
  --start 2026-04-11T14:00:00+08:00 \
  --end 2026-04-11T15:00:00+08:00 \
  --staff-ids 011533646711841213 621942314220944463 \
  --location "3楼大会议室" \
  --description "讨论本周进度" \
  --reminders 15 30 \
  --online-meeting
```

| 参数 | 必填 | 说明 |
|------|------|------|
| --summary | 是 | 日程标题（最大 2048 字符） |
| --start | 是 | 开始时间（ISO-8601）或日期（全天日程 yyyy-MM-dd） |
| --end | 否 | 结束时间/日期（全天日程自动 T+1，无需手动加） |
| --description | 否 | 日程描述（最大 5000 字符） |
| --location | 否 | 地点名称 |
| --all-day | 否 | 标记为全天日程（start 为日期格式时自动识别，无需显式传） |
| --staff-ids | 否 | 参与者 staff_id 列表（空格分隔，最多 500 人） |
| --reminders | 否 | 提前提醒分钟数列表（不传=默认15分钟；传空=不提醒） |
| --online-meeting | 否 | 同时创建钉钉视频会议 |
| --no-push | 否 | 不发钉钉推送通知（App 内弹窗） |
| --no-chat | 否 | 不发单聊卡片通知 |

限制：每用户每天限创建 100 个日程。

#### list-events — 查询日程列表

```
python tools/dingtalk_calendar.py list-events \
  --time-min 2026-04-10T00:00:00+08:00 \
  --time-max 2026-04-17T00:00:00+08:00 \
  --max-results 20
```

| 参数 | 必填 | 说明 |
|------|------|------|
| --time-min | 否 | 开始时间最小值（ISO-8601），与 time-max 最大差值一年 |
| --time-max | 否 | 开始时间最大值 |
| --max-results | 否 | 最大返回数（默认 100，最大 100） |
| --next-token | 否 | 分页游标 |

#### get-event — 查询单个日程详情

```
python tools/dingtalk_calendar.py get-event --event-id <eventId>
```

#### view-events — 查询日程视图

与 list-events 参数相同，区别：会将循环日程展开为查询区间内的所有实例。

```
python tools/dingtalk_calendar.py view-events \
  --time-min 2026-04-10T00:00:00+08:00 \
  --time-max 2026-04-17T00:00:00+08:00
```

#### update-event — 修改日程

仅组织者可修改（先 `get-event` 确认 organizer.self=true）。只传需要修改的字段。

```
python tools/dingtalk_calendar.py update-event \
  --event-id <eventId> \
  --summary "新标题" \
  --location "5楼小会议室"
```

| 参数 | 必填 | 说明 |
|------|------|------|
| --event-id | 是 | 日程 ID |
| --summary | 否 | 新标题 |
| --start | 否 | 新开始时间（传日期格式自动识别为全天） |
| --end | 否 | 新结束时间（全天日程自动 T+1） |
| --description | 否 | 新描述 |
| --location | 否 | 新地点 |
| --all-day | 否 | 设为全天日程（start 为日期格式时自动识别） |
| --no-all-day | 否 | 取消全天，改为定时日程（与 --all-day 互斥） |
| --no-push | 否 | 不发钉钉推送通知 |
| --no-chat | 否 | 不发单聊卡片通知 |

#### delete-event — 删除日程

**危险操作，行为取决于当前用户身份：**
- **组织者删除（取消会议）**：日程从所有参与者日历中删除，所有人收到取消通知，不可撤销
- **参与者删除（退出会议）**：仅从自己日历移除，其他参与者不受影响

执行前必须先 `get-event` 确认用户身份，并向用户说明后果。

```
python tools/dingtalk_calendar.py delete-event --event-id <eventId> --push-notification
```

### 参与者管理

#### list-attendees — 获取参与者列表

```
python tools/dingtalk_calendar.py list-attendees --event-id <eventId>
```

返回 attendees 数组，每项含 id/displayName/responseStatus（needsAction/accepted/declined/tentative）。

#### add-attendees — 添加参与者

```
python tools/dingtalk_calendar.py add-attendees \
  --event-id <eventId> \
  --staff-ids 011533646711841213
```

每次最大 500 人。`--push-notification` 控制弹窗提醒，`--chat-notification` 控制单聊卡片提醒。

#### remove-attendees — 移除参与者

仅组织者可操作。

```
python tools/dingtalk_calendar.py remove-attendees \
  --event-id <eventId> \
  --staff-ids 011533646711841213
```

#### respond-event — 响应日程邀请

```
python tools/dingtalk_calendar.py respond-event \
  --event-id <eventId> \
  --status accepted
```

status 可选值：`accepted`（接受）/ `declined`（拒绝）/ `tentative`（暂定）/ `needsAction`（未操作）

### 忙闲查询

#### query-schedule — 查询用户忙闲

```
python tools/dingtalk_calendar.py query-schedule \
  --staff-ids 621942314220944463 011533646711841213 \
  --start 2026-04-10T00:00:00+08:00 \
  --end 2026-04-11T00:00:00+08:00
```

最多查询 20 个用户。返回每个用户的忙闲记录列表（空=该时段空闲），status 为 BUSY 或 TENTATIVE。

**注意**：`query-schedule` 成功仅代表该 staff_id 已绑定了某个 unionId，**不代表绑定的身份正确**（可能存在 A 的 staff_id 误绑了 B 的 unionId 的情况）。因此不能仅凭 query-schedule 成功就确认"某人已授权"，在后续写操作前仍需按参与者解析流程的步骤 2 验证身份。

### 会议室

#### list-rooms — 列出可用会议室

```
python tools/dingtalk_calendar.py list-rooms --max-results 50
```

返回 roomId / roomName / roomCapacity / roomLocation / roomLabels（电视/电话/投影仪/白板/视频会议）。

#### query-room-schedule — 查询会议室忙闲

```
python tools/dingtalk_calendar.py query-room-schedule \
  --room-ids <roomId1> <roomId2> \
  --start 2026-04-10T09:00:00+08:00 \
  --end 2026-04-10T18:00:00+08:00
```

建议不超过 5 个 roomId。返回每个会议室的忙闲记录（空=该时段空闲）。

#### book-room — 预定会议室

将会议室绑定到已有日程（需先 create-event 创建日程）。

```
python tools/dingtalk_calendar.py book-room \
  --event-id <eventId> \
  --room-ids <roomId>
```

一个日程最多 5 个会议室。

#### cancel-room — 取消预定会议室

```
python tools/dingtalk_calendar.py cancel-room \
  --event-id <eventId> \
  --room-ids <roomId>
```

## 典型场景

### 会议室选择流程（通用，被下方多个场景引用）

以下 3 步是选择会议室的通用流程，无论是新建日程还是给已有日程加会议室，都走这个流程。

**Step A — 收集需求**

选会议室前需要 3 项信息，**先从上下文推断已知项，只问真正缺的**：

- **位置**：用户所在城市 + 建筑 + 楼层（如"北京北辰泰岳26层"）。优先从用户记忆中获取；记忆中没有才问
- **人数**：参会人数（含发起人自己）。通常可从参与者列表直接算出，不需要问
- **设备需求**：是否需要视频会议/电视/白板等。可从会议目的推断——普通会议/线下会议默认无特殊设备需求，不需要问；仅当会议目的明确涉及演示、培训等场景时才确认

原则：**能推断的不要问，一次只问一个最关键的缺失信息**

**Step B — 筛选 & 查忙闲**

1. `list-rooms` 获取全部会议室（含 roomLocation / roomCapacity / roomLabels）
2. 按优先级逐层筛选：
   - **城市**：必须匹配，**绝对不能跨城市推荐会议室**
   - **建筑**：优先匹配用户所在建筑，建筑全满时按就近关系扩展（见下方规则）
   - **容量匹配**：roomCapacity ≥ 参会人数；同楼层有多个空闲时优先选容量接近的（避免 3 人占 20 人会议室），但同楼层只剩容量偏大的房间时仍然推荐（优于换楼层）
   - **楼层就近**：同楼层 > 相邻 1 层 > 相邻 2 层（更远需提示用户确认）
   - **设备匹配**：roomLabels 满足设备需求
3. `query-room-schedule` 检查候选会议室忙闲（每次最多 5 个，优先查同楼层的）

建筑就近关系（同城市内）：
- 暖山生活A座 ↔ 暖山生活B座：相邻建筑，可互相推荐
- 北辰泰岳：距暖山生活稍远，仅作为最后备选
- 其他建筑之间无就近关系，不自动推荐

**Step C — 推荐**

- **唯一合适** → 直接推荐，说明位置/容量/设备
- **多个合适** → 列出 top 2-3 个，标注各自优劣（如"同楼层但略小" vs "隔壁层但设备更全"），让用户选
- **同楼层无空闲** → 同时提供两个维度的备选，让用户选：
  - **调时间**：查同楼层会议室在前后相邻时段（如前/后 30 分钟或 1 小时）的空闲情况；如有参与者，须同时 `query-schedule` 确认参会人在备选时段也有空，再推荐
  - **换楼层**：扩展到相邻楼层查询同一时段的空闲会议室
  - 向用户呈现："X 层 Y 时段会议室已满。有两个方案：① 同楼层 Z 时段有空闲；② 相邻 N 层同时段有空闲"，由用户决定
- **同建筑全满** → 按就近关系自动查相邻建筑（如 A座满 → 查B座），告知用户；无就近建筑或就近也满 → 告知用户无可用会议室，不再自动扩展（除非用户主动要求查看其他建筑）

### 创建带会议室的日程

适用：新建日程时用户需要会议室（由"会议室主动引导"触发，或用户直接要求）。

1. 走 Step A-C 选定会议室
2. `create-event` 创建日程（含参与者）
3. `book-room` 绑定选定的会议室

### 给已有日程加会议室

适用：日程已创建，用户追问"帮这个会议订个会议室"/"有推荐的会议室吗"。

1. 从上下文获取 event-id 和日程时间、参与者人数
2. 走 Step A-C 选定会议室
3. `book-room --event-id <eventId> --room-ids <roomId>` 绑定会议室

### 查看某人是否有空

1. 通过 `/api/resolve-user` 解析人名 → staff_id
2. `query-schedule --staff-ids <id> --start ... --end ...`
3. 根据返回的忙闲记录告知用户

### 安排多人会议

1. 解析所有参与者名字 → staff_ids
2. `query-schedule` 查询所有人忙闲 → 找出共同空闲时段
3. 向用户推荐时段，确认后创建日程
4. 如果触发了"会议室主动引导"或用户要求会议室，按"创建带会议室的日程"流程执行（Step A-C → create-event → book-room）；否则直接 `create-event`
