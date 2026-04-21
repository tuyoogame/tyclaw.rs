---
name: 引力引擎
description: 查询引力引擎微信/抖音小游戏排行榜、App趋势、竞品分析
triggers:
  - 引力
  - 引力引擎
  - 引力排行榜
  - 小游戏排行
  - 微信小游戏榜
  - 抖音小游戏榜
  - 人气榜
  - 畅销榜
  - 畅玩榜
  - 新游榜
  - 霸榜
  - 设置引力
  - 设置引力凭证
  - 引力账号
tool: tools/gravity_query.py
default: false
credentials:
  key: gravity
  display_name: 引力引擎
  setup: sms
  fields:
    - {name: phone, label: 手机号}
    - {name: jwt, label: JWT Token, secret: true}
    - {name: expire_time, label: 过期时间戳(ms)}
  expiry:
    warn_before: 1d
    action: regenerate
---
# 引力引擎排行榜查询

查询引力引擎 (rank.gravity-engine.com) 的微信/抖音小游戏排行榜数据，包括人气榜、畅销榜、畅玩榜、新游榜，支持 App 排名趋势和竞品分析。

> 本文件位于 `skills/gravity-rank/`，下文中的相对路径均基于项目根目录。

## 凭证配置

引力引擎使用短信验证码登录，需要两步：

```bash
# 第1步：发送验证码
python3 tools/gravity_query.py send-code --phone "15120071654"

# 第2步：输入收到的验证码登录
python3 tools/gravity_query.py login --phone "15120071654" --code 123456
```

清除凭证：

```bash
python3 tools/gravity_query.py clear-credentials
```

JWT 有效期 **7 天**，过期后需重新发送验证码登录。

当用户说「设置引力引擎凭证」时：
1. 询问用户手机号，并告知收到后会发送短信验证码
2. 收到手机号后调用 `send-code` 发送验证码，提示用户查看短信
3. 收到验证码后调用 `login` 完成登录

**过期续期场景**：如果是凭证过期提醒触发的续期，需先询问用户"是否现在重新登录？"，确认后再发送验证码。禁止自动发送

## 工具用法

`tools/gravity_query.py`（从 TyClaw 项目根目录运行）

### 查询排行榜

```bash
# 微信小游戏人气榜（默认）
python3 tools/gravity_query.py rank

# 微信小游戏畅销榜
python3 tools/gravity_query.py rank --rank-type bestseller

# 微信小游戏畅玩榜
python3 tools/gravity_query.py rank --rank-type most_played

# 抖音小游戏热门榜
python3 tools/gravity_query.py rank --rank-genre dy_minigame

# 抖音小游戏畅销榜
python3 tools/gravity_query.py rank --rank-type bestseller --rank-genre dy_minigame

# 抖音小游戏新游榜
python3 tools/gravity_query.py rank --rank-type fresh_game --rank-genre dy_minigame

# 指定日期、分页
python3 tools/gravity_query.py rank --date 2026-04-18 --page 2 --page-size 50

# 按游戏分类筛选
python3 tools/gravity_query.py rank --game-type 休闲

# JSON 输出
python3 tools/gravity_query.py --format json rank
```

### 查询 App 排名趋势

```bash
# 查询指定 App 最近7天的排名趋势（需先通过 search 获取 app_id）
python3 tools/gravity_query.py trend --app-id 34632349 --start-date 2026-04-13 --end-date 2026-04-19

# 指定榜单类型
python3 tools/gravity_query.py trend --app-id 34632349 --start-date 2026-04-13 --end-date 2026-04-19 --rank-type-list popularity,bestseller,most_played

# 抖音平台
python3 tools/gravity_query.py trend --app-id 34812404 --start-date 2026-04-13 --end-date 2026-04-19 --rank-genre dy_minigame
```

### 搜索应用

```bash
# 按名称搜索
python3 tools/gravity_query.py search --keyword "钓鱼"

# 只搜索微信小游戏
python3 tools/gravity_query.py search --keyword "三国" --app-os 3

# 只搜索抖音小游戏
python3 tools/gravity_query.py search --keyword "三国" --app-os 6
```

### 搜索发行商

```bash
python3 tools/gravity_query.py publisher --keyword "腾讯"
```

### 竞品趋势

```bash
# 查看某 App 所在榜单位置的竞品排名
python3 tools/gravity_query.py competition --app-id 34632349 --rank-type popularity --rank-genre wx_minigame --start-date 2026-04-13 --end-date 2026-04-19
```

## 参数速查

### 排行榜参数

| 参数 | 说明 | 可选值 | 默认值 |
|------|------|--------|--------|
| `--rank-type` | 榜单类型 | popularity/bestseller/most_played/fresh_game | popularity |
| `--rank-genre` | 平台 | wx_minigame/dy_minigame | wx_minigame |
| `--page` | 页码 | 1-30 | 1 |
| `--page-size` | 每页条数 | 1-50 | 20 |
| `--date` | 日期 | YYYY-MM-DD | 今天 |
| `--game-type` | 游戏分类 | 休闲/竞技/棋牌/角色/策略等 | 不限 |

### 榜单类型映射

| rank_type | 微信小游戏 | 抖音小游戏 | 用户表述 |
|-----------|-----------|-----------|---------|
| popularity | 人气榜 | 热门榜 | "人气""热门""排名" |
| bestseller | 畅销榜 | 畅销榜 | "畅销""收入""赚钱" |
| most_played | 畅玩榜 | ❌ 不适用 | "畅玩""活跃" |
| fresh_game | ❌ 不适用 | 新游榜 | "新游""新上线" |

### 平台映射

| 用户表述 | rank_genre | app_os |
|---------|-----------|--------|
| 微信小游戏 / 微信 / wx | wx_minigame | 3 |
| 抖音小游戏 / 抖音 / 字节 / dy | dy_minigame | 6 |

用户未指定平台时默认查**微信小游戏**。

## 自然语言映射示例

**示例 1**: "看一下今天微信小游戏人气榜"

```bash
python3 tools/gravity_query.py rank --rank-type popularity --rank-genre wx_minigame
```

**示例 2**: "抖音畅销榜前50"

```bash
python3 tools/gravity_query.py rank --rank-type bestseller --rank-genre dy_minigame --page-size 50
```

**示例 3**: "搜一下无尽冬日"

```bash
python3 tools/gravity_query.py search --keyword "无尽冬日"
```

**示例 4**: "无尽冬日最近一周排名趋势"

```bash
python3 tools/gravity_query.py trend --app-id 34632349 --start-date 2026-04-13 --end-date 2026-04-19 --rank-type-list popularity,bestseller
```

**示例 5**: "看看无尽冬日在畅销榜的竞品"

```bash
python3 tools/gravity_query.py competition --app-id 34632349 --rank-type bestseller --rank-genre wx_minigame --start-date 2026-04-13 --end-date 2026-04-19
```

## 返回数据说明

### 排行榜字段

| 字段 | 说明 |
|------|------|
| ranking | 排名 |
| app_info.app_name | 应用名称 |
| app_info.publisher_name | 发行商 |
| app_info.game_type_main_name | 游戏大类 |
| change | 排名变化（正数下降，负数上升） |
| change_label.first_msg | 霸榜天数 |

### App 搜索字段

| 字段 | 说明 |
|------|------|
| id | App ID（用于 trend/competition 查询） |
| name | 应用名称 |
| publisher_name | 发行商 |
| app_os | 平台：3=微信，6=抖音 |
| game_type_main_name | 游戏大类 |

## 安全约束

1. 此工具仅做只读查询，无写入操作
2. JWT Token 自动管理，7 天过期后需重新登录
