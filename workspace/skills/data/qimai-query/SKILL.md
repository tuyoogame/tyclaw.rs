---
name: 七麦数据查询
description: 查询七麦数据平台的APP榜单排名、搜索应用、查看APP详情和版本记录
triggers:
  - 七麦
  - 七麦数据
  - qimai
  - APP榜单
  - 应用排名
  - 免费榜
  - 付费榜
  - 畅销榜
  - APP排行
  - 应用商店排名
  - 关键词排名
  - 设置七麦
  - 设置七麦凭证
  - 七麦账号
tool: tools/qimai_query.py
default: false
credentials:
  key: qimai
  display_name: 七麦数据
  setup: manual
  fields:
    - {name: email, label: 七麦登录邮箱}
    - {name: password, label: 登录密码, secret: true}
---
# 七麦数据查询

通过七麦数据 (qimai.cn) API 查询 iOS/Android 应用榜单排名、搜索应用、查看应用详情。

> 本文件位于 `skills/qimai-query/`，下文中的相对路径均基于项目根目录。

## 前置依赖

```bash
pip install requests
```

## 凭证配置

使用前需先设置七麦凭证（七麦平台的登录邮箱和密码）：

```bash
python3 tools/qimai_query.py setup --email "user@example.com" --password "your-password"
```

清除凭证：

```bash
python3 tools/qimai_query.py clear-credentials
```

凭证通过环境变量 `_TYCLAW_QIMAI_EMAIL` / `_TYCLAW_QIMAI_PASSWORD` 自动注入，也可通过 `--email` / `--password` 命令行参数手动传入。

当用户说「设置七麦凭证」「七麦账号」时，询问用户提供七麦数据平台 (qimai.cn) 的登录邮箱和密码，然后调用 `setup`。

## 工具用法

`tools/qimai_query.py`（从 TyClaw 项目根目录运行）

### APP 榜单查询

```bash
# 游戏免费榜（默认）
python3 tools/qimai_query.py rank

# 游戏畅销榜
python3 tools/qimai_query.py rank --brand grossing --genre game

# 应用免费榜
python3 tools/qimai_query.py rank --brand free --genre app

# 付费榜
python3 tools/qimai_query.py rank --brand paid

# 指定分类
python3 tools/qimai_query.py rank --genre strategy   # 策略游戏
python3 tools/qimai_query.py rank --genre role        # 角色扮演
python3 tools/qimai_query.py rank --genre action      # 动作游戏
python3 tools/qimai_query.py rank --genre casual      # 休闲游戏

# iPad / Android
python3 tools/qimai_query.py rank --device ipad
python3 tools/qimai_query.py rank --device android

# 美国/日本/韩国榜单
python3 tools/qimai_query.py rank --country us
python3 tools/qimai_query.py rank --country jp
python3 tools/qimai_query.py rank --country kr

# 指定日期和页码
python3 tools/qimai_query.py rank --date 2026-04-10 --page 2
```

### 搜索 APP

```bash
python3 tools/qimai_query.py search --keyword "三国"
python3 tools/qimai_query.py search --keyword "王者荣耀" --country us
```

### 查询 APP 详情

```bash
python3 tools/qimai_query.py app-info --app-id 6503432459
python3 tools/qimai_query.py app-info --app-id 989673964 --country us
```

### 查询 APP 排名趋势

```bash
python3 tools/qimai_query.py app-rank --app-id 6503432459
python3 tools/qimai_query.py app-rank --app-id 6503432459 --brand grossing
```

### 查询关键词排名（需登录）

```bash
python3 tools/qimai_query.py keyword-rank --app-id 6503432459
```

### 查询版本记录

```bash
python3 tools/qimai_query.py version-history --app-id 6503432459
```

## 参数速查

全局参数：

| 参数 | 说明 |
|---|---|
| `--email` | 登录邮箱（可选，优先于环境变量） |
| `--password` | 登录密码（可选，优先于环境变量） |

### 榜单参数

| 参数 | 说明 | 可选值 | 默认值 |
|---|---|---|---|
| `--brand` | 榜单类型 | free/paid/grossing | free |
| `--device` | 设备类型 | iphone/ipad/mac/android | iphone |
| `--country` | 国家代码 | cn/us/jp/kr 等 | cn |
| `--genre` | 分类 | 见下表，也可传数字 ID | game |
| `--date` | 日期 | YYYY-MM-DD | 今天 |
| `--page` | 页码 | 1-10，每页 20 条 | 1 |

### 分类 genre 速查

| genre | 中文名 | 数字 ID |
|---|---|---|
| all | 全部 | 36 |
| app | 全部应用 | 6000 |
| game | 全部游戏 | 6014 |
| action | 动作游戏 | 7001 |
| adventure | 冒险游戏 | 7002 |
| casual | 休闲游戏 | 7003 |
| card | 卡牌游戏 | 7005 |
| puzzle | 益智解谜 | 7012 |
| racing | 赛车游戏 | 7013 |
| role | 角色扮演 | 7014 |
| simulation | 模拟游戏 | 7015 |
| sports | 体育游戏 | 7016 |
| strategy | 策略游戏 | 7017 |

## 返回数据说明

### 榜单返回字段

| 字段 | 说明 |
|---|---|
| `index` | 分类内排名 |
| `appInfo.appName` | 应用名称 |
| `appInfo.appId` | App Store ID |
| `appInfo.publisher` | 开发商 |
| `class.ranking` | 总榜排名 |
| `change` | 排名变化 |
| `is_ad` | 是否推广 |

### 搜索返回字段

| 字段 | 说明 |
|---|---|
| `popularity` | 关键词热度 |
| `totalNum` | 结果总数 |
| `appList[].appInfo.appName` | 应用名称 |
| `appList[].appInfo.appId` | App Store ID |
| `appList[].genre` | 分类 |

### APP 详情返回字段

| 字段 | 说明 |
|---|---|
| `appInfo.appname` | 应用名称 |
| `appInfo.genre.name` | 分类名 |
| `appInfo.genre.rank` | 分类排名 |
| `appInfo.publisher` | 开发商 |
| `appInfo.releaseTime` | 上架日期 |
| `appInfo.yesterdayDownload` | 昨日预估下载量 |
| `appInfo.current_version_user_rating` | 评分 |
| `appInfo.rateDetail.all.ratingCount` | 评分总数 |

## 认证流程

1. API 请求需携带 `analysis` 加密参数
2. 加密算法：params 值排序拼接 → Base64 → 拼接 URL 路径和时间戳 → XOR 加密 → Base64
3. 部分接口（如关键词排名）需要登录后的 cookie
4. Session 缓存到 `~/.cache/qimai_session.json`，2 小时有效
