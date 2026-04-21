---
name: Discord社区
description: Discord 社区数据查询：频道消息、成员列表、线程
triggers:
  - Discord
  - DC
  - DC社区
  - DC频道
  - DC消息
  - DC成员
  - DC数据
  - 社区数据
  - 社区监控
  - 玩家社区
  - 设置Discord
tool: tools/discord_api.py
default: false
credentials:
  section: discord
  fields:
    - name: bot_token
      label: Bot Token
      help: 从 Discord Developer Portal 的 Bot 页面生成
    - name: guild_id
      label: Guild ID
      help: 右键 Discord 服务器 → 复制服务器 ID
---
# Discord 社区数据

通过 Discord REST API 查询社区数据：频道列表、历史消息、成员信息、活跃线程。凭证通过环境变量注入（per-user credentials.yaml）。

> 本文件位于 `skills/discord/`，下文中的相对路径均基于项目根目录。

## 前置依赖

```bash
pip install requests
```

## 凭证配置

使用前需先设置 Discord Bot Token 和 Guild ID：

```bash
python3 tools/discord_api.py setup --bot-token "YOUR_BOT_TOKEN" --guild-id "YOUR_GUILD_ID"
```

Bot Token 从 [Discord Developer Portal](https://discord.com/developers/applications) → 你的应用 → Bot 页面获取。Guild ID 在 Discord 客户端开启开发者模式后，右键服务器 → 复制服务器 ID。

## 工具用法

`tools/discord_api.py`（从 TyClaw 项目根目录运行）

### 获取服务器信息

```bash
python3 tools/discord_api.py get-guild
```

### 列出频道

```bash
# 所有频道
python3 tools/discord_api.py list-channels

# 只看文字频道
python3 tools/discord_api.py list-channels --type 0

# 只看论坛频道
python3 tools/discord_api.py list-channels --type 15
```

### 获取频道消息

```bash
# 最近 50 条
python3 tools/discord_api.py get-messages --channel-id "1384476930090340362"

# 最近 100 条
python3 tools/discord_api.py get-messages --channel-id "1384476930090340362" --limit 100

# 某条消息之后的新消息（定时轮询用）
python3 tools/discord_api.py get-messages --channel-id "1384476930090340362" --after "1494153919952322581"

# 某条消息之前的历史消息
python3 tools/discord_api.py get-messages --channel-id "1384476930090340362" --before "1494153919952322581"
```

### 成员管理

```bash
# 列出成员（最多 1000）
python3 tools/discord_api.py list-members --limit 100

# 翻页（传上一页最后一个 user_id）
python3 tools/discord_api.py list-members --limit 100 --after "123456789"

# 按名称搜索成员
python3 tools/discord_api.py search-members --query "suike"
```

### 活跃线程

```bash
python3 tools/discord_api.py list-threads
```

## 参数速查

### 全局参数

| 参数 | 说明 |
|------|------|
| `--guild-id` | 可选，覆盖默认服务器 ID |

### 频道类型 ID

| ID | 类型 |
|----|------|
| 0 | 文字频道 |
| 2 | 语音频道 |
| 4 | 分类 |
| 5 | 公告频道 |
| 13 | Stage 频道 |
| 15 | 论坛频道 |

### get-messages 参数

| 参数 | 说明 | 默认值 |
|------|------|--------|
| `--channel-id` | 频道或线程 ID | （必填） |
| `--limit` | 消息数量（1-100） | 50 |
| `--before` | 此消息 ID 之前 | - |
| `--after` | 此消息 ID 之后 | - |
| `--around` | 此消息 ID 前后 | - |

### 消息输出字段

| 字段 | 说明 |
|------|------|
| `id` | 消息 ID |
| `author_id` | 发送者用户 ID |
| `author_name` | 发送者用户名 |
| `author_bot` | 是否是机器人 |
| `content` | 消息内容 |
| `timestamp` | 发送时间 |
| `reactions` | 表情反应列表 |
| `reply_to` | 回复的原始消息（如有） |
| `thread_id` | 关联的线程 ID（如有） |

## 典型使用场景

### 定时监控频道新消息

创建定时任务，每小时拉取 bug-report 频道的新消息并汇总到钉钉：

1. 首次拉取记录最新消息 ID
2. 后续使用 `--after` 参数只拉增量
3. AI 分析消息内容，分类汇总

### 社区数据日报

创建每日定时任务：

1. `list-channels` 获取所有频道
2. 对关键频道 `get-messages --limit 100` 拉取当日消息
3. AI 统计发言人数、活跃度、话题热度
4. 结果推送到钉钉

### 用户画像分析

1. `list-members` 遍历获取全部成员
2. 对活跃频道 `get-messages` 统计发言频次
3. AI 标记高活跃用户、潜在 Mod、异常用户

## 注意事项

- Discord API 有速率限制（Rate Limit），大批量拉取时注意控制频率
- `list-members` 需要 Bot 开启 **SERVER MEMBERS** Privileged Intent
- `get-messages` 读取消息内容需要 Bot 开启 **MESSAGE CONTENT** Privileged Intent
- 当前仅支持只读操作，写入接口（发消息、创建线程等）后续按需添加
