---
name: 定时任务
description: 设置定时任务，让机器人在指定时间自动执行并推送结果
triggers:
  - 每天帮我
  - 定时执行
  - 定时任务
  - 提醒我
  - 我的定时任务
  - 查看定时
  - 取消定时
  - 删除定时
  - 暂停定时
  - 恢复定时
  - 修改定时
  - 调整定时
  - 更新定时
  - 定时任务调整
tool: tools/scheduler_tool.py
default: true
---
# 定时任务管理

## 职责

管理用户的定时任务：创建、查看、修改、删除、启停。
定时任务到点后由后台调度器自动执行，结果推送到设定时的会话中。

## 意图判断

- 创建意图：每天/每周/工作日XX点帮我做YY、定时执行XX、提醒我XX → 执行「创建流程」
- 查看意图：我的定时任务、查看定时、有哪些定时 → 执行「查看流程」
- 修改意图：修改/调整/更新定时任务的时间或内容 → 执行「修改流程」
- 删除意图：取消/删除定时任务XX → 执行「删除流程」
- 启停意图：暂停/恢复定时任务XX → 执行「启停流程」

---

## 工具

路径：`tools/scheduler_tool.py`

```bash
# 添加（--end-at 可选，ISO 格式截止时间，到期自动停用）
python tools/scheduler_tool.py add --name "任务名称" --cron "分 时 日 月 周" --message "要执行的消息" [--end-at "2025-04-11T10:00:00"]

# 查看
python tools/scheduler_tool.py list

# 修改（只传需要改的字段，未传的保持不变；--end-at 传空字符串可清除截止时间）
python tools/scheduler_tool.py update --id <任务ID> [--name "新名称"] [--cron "新cron"] [--message "新消息"] [--end-at "2025-04-11T10:00:00"]

# 删除
python tools/scheduler_tool.py remove --id <任务ID>

# 启停
python tools/scheduler_tool.py toggle --id <任务ID>
```

---

## 一、创建流程

### Step 1: 解析时间

将用户的自然语言时间转为标准 5 段 cron 表达式（分 时 日 月 周）：

| 用户说法 | cron 表达式 |
|---------|------------|
| 每天9点 | `0 9 * * *` |
| 每天9:30 | `30 9 * * *` |
| 工作日9点 | `0 9 * * 1-5` |
| 每周一9点 | `0 9 * * 1` |
| 每周一和周五18点 | `0 18 * * 1,5` |
| 每天上午10点和下午3点 | `0 10,15 * * *` |
| 每小时 | `0 * * * *` |
| 每两小时（偶数小时） | `0 */2 * * *` |

周几映射：周一=1，周二=2，...，周日=0 或 7

**单次执行**：用户说"30分钟后帮我做xxx"、"今天下午3点提醒我xxx"等一次性任务时，cron 设为具体的 分 时 日 月，`--end-at` 设为执行时间后 2 分钟，执行完自动停用。

例如当前是 2025-04-10 14:00，用户说"30分钟后帮我查数据"：
- cron: `30 14 10 4 *`
- end-at: `2025-04-10T14:32:00`

### Step 2: 确定消息内容

提取用户想让机器人做的事情作为 message。message 应该是一条完整的指令，就像用户直接发给机器人一样。

例如用户说"每天9点帮我查一下昨天的投放消耗"，则：
- cron: `0 9 * * *`
- message: `帮我查一下昨天的投放消耗`

### Step 3: 判断是否有截止时间

如果用户指定了结束/截止时间（如"到明天上午10点结束"、"只执行到周五"），将其转为 ISO 格式，通过 `--end-at` 传入。未指定则不传。

### Step 4: 调用工具

```bash
# 无截止时间
python tools/scheduler_tool.py add --name "每日投放数据" --cron "0 9 * * *" --message "帮我查一下昨天的投放消耗"

# 有截止时间
python tools/scheduler_tool.py add --name "临时监控" --cron "*/30 * * * *" --message "三冰抖小空耗 1000" --end-at "2025-04-11T10:00:00"
```

### Step 5: 确认回复

```
定时任务已创建！

- 名称：每日投放数据
- 执行时间：每天 09:00
- 执行内容：帮我查一下昨天的投放消耗
- 结束时间：（如有则展示，如"2025-04-11 10:00 后自动停用"）

到时间后会自动执行并把结果发给你。
发送「我的定时任务」可查看所有定时任务。
```

---

## 二、查看流程

```bash
python tools/scheduler_tool.py list
```

将工具输出格式化后回复用户。

---

## 三、修改流程

### Step 1: 定位任务

先执行 `list` 找到用户要修改的任务 ID。如果用户提供了 ID 则直接使用。

### Step 2: 执行修改

只传需要更改的字段，未传的保持不变：

```bash
# 只改执行时间
python tools/scheduler_tool.py update --id <任务ID> --cron "5 15 * * *"

# 只改名称
python tools/scheduler_tool.py update --id <任务ID> --name "新名称"

# 改时间和消息
python tools/scheduler_tool.py update --id <任务ID> --cron "5 15 * * *" --message "新消息"

# 设置/修改截止时间
python tools/scheduler_tool.py update --id <任务ID> --end-at "2025-04-12T18:00:00"

# 清除截止时间（改为永久执行）
python tools/scheduler_tool.py update --id <任务ID> --end-at ""
```

如果用户要修改多个任务，逐个调用 update。

### Step 3: 确认回复

```
定时任务已更新！

- 名称：XXX
- 执行时间：每天 15:05
- 执行内容：XXX
```

---

## 四、删除流程

### Step 1: 定位任务

先执行 `list` 找到用户要删除的任务 ID，根据名称或描述匹配。
如果无法确定是哪个，列出所有任务让用户选择。

### Step 2: 执行删除

```bash
python tools/scheduler_tool.py remove --id <任务ID>
```

### Step 3: 确认回复

```
定时任务「XXX」已删除。
```

---

## 五、启停流程

```bash
python tools/scheduler_tool.py toggle --id <任务ID>
```

回复当前状态（已暂停 / 已恢复）。

---

## 安全约束

1. 只能管理当前用户自己的定时任务
2. staff_id 从环境变量 `TYCLAW_SENDER_STAFF_ID` 获取，禁止硬编码
3. 不要创建间隔过于频繁的定时任务（最短间隔建议 1 小时）
