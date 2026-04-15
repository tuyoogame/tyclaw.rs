---
name: Sensor Tower数据
description: 查询 Sensor Tower 移动应用市场数据（下载量、收入、排行榜、活跃用户）
triggers:
  - Sensor Tower
  - sensortower
  - 下载量
  - 收入估算
  - App 排行榜
  - 应用排行
  - DAU
  - MAU
  - 活跃用户
  - 应用市场数据
  - 移动应用数据
  - App Store 排名
  - Google Play 排名
  - 设置 Sensor Tower
  - 设置ST凭证
tool: tools/st_query.py
default: false
credentials:
  section: st
  fields:
    - name: token
      label: API Token
      help: 从 Sensor Tower 后台「API 令牌」页面生成
---
# Sensor Tower 数据查询

通过 Sensor Tower API 查询移动应用市场情报数据。所有 API 调用经 Bot 侧代理（含缓存 + 频率限制），共享缓存可跨用户命中。

> 本文件位于 `skills/st-query/`，下文中的相对路径均基于项目根目录。

## 前置依赖

```bash
pip install requests
```

## 凭证配置

使用前需先设置 Sensor Tower API Token（从 ST 后台「API 令牌」页面生成）：

```bash
python3 tools/st_query.py setup --token "YOUR_API_TOKEN"
```

API 配额为公司账号共用（3,000 次/月），Bot 侧实施频率限制（全局 100 次/天，单人 30 次/天），仅 cache-miss 的实际 API 调用计数。

## 工具用法

`tools/st_query.py`（从 TyClaw 项目根目录运行）

### 搜索 App / 发行商

```bash
# 按名称搜索 App
python3 tools/st_query.py search --term "Candy Crush"

# 搜索发行商
python3 tools/st_query.py search --term "Lilith" --entity-type publisher

# 指定平台
python3 tools/st_query.py search --term "原神" --os ios --limit 5
```

### 下载量 & 收入估算

```bash
# 查询单个 App 的全球数据（需先通过 search 获取 app_id）
python3 tools/st_query.py sales \
  --app-ids "553834731" \
  --os ios \
  --countries "US,JP,CN" \
  --start-date 2026-03-01 --end-date 2026-03-31 \
  --date-granularity daily

# 多个 App 对比
python3 tools/st_query.py sales \
  --app-ids "553834731,1234567890" \
  --os ios \
  --countries "WW" \
  --start-date 2026-01-01 --end-date 2026-03-31 \
  --date-granularity monthly
```

> 收入数据以**美分**返回，需除以 100 得到美元金额。

### App 排行榜

```bash
# iOS 游戏收入 Top 20（本月，全球）
python3 tools/st_query.py top-charts \
  --os ios --measure revenue --category 6014 --regions WW

# Android 下载量 Top 50（美国，按周）
python3 tools/st_query.py top-charts \
  --os android --measure units --category 6014 \
  --regions US --time-range week --limit 50

# 指定日期
python3 tools/st_query.py top-charts \
  --os unified --measure revenue --category 6014 \
  --regions US --date 2026-03-01
```

### App 详情

```bash
python3 tools/st_query.py app-info --app-ids "553834731" --os ios
```

### 活跃用户（DAU/WAU/MAU）

```bash
python3 tools/st_query.py usage \
  --app-ids "553834731" \
  --os ios \
  --countries "US" \
  --start-date 2026-01-01 --end-date 2026-03-31 \
  --date-granularity monthly
```

### 参数速查

通用参数：

| 参数 | 说明 | 默认值 |
|---|---|---|
| `--os` | 平台：ios / android / unified | unified |
| `--countries` | 国家代码，逗号分隔（WW=全球） | WW |
| `--start-date` | 开始日期 (YYYY-MM-DD) | - |
| `--end-date` | 结束日期 (YYYY-MM-DD) | - |
| `--date-granularity` | 粒度：daily/weekly/monthly/quarterly | daily |
| `--limit` | 返回条数 | 10-20 |

search 参数：

| 参数 | 说明 | 默认值 |
|---|---|---|
| `--term` | 搜索关键词 | （必填） |
| `--entity-type` | app / publisher | app |

top-charts 参数：

| 参数 | 说明 | 默认值 |
|---|---|---|
| `--measure` | revenue / units / DAU / WAU / MAU | revenue |
| `--category` | 分类 ID | （必填） |
| `--regions` | 地区代码 | WW |
| `--time-range` | day / week / month / quarter | month |
| `--date` | 起始日期 | 当月1号 |

## 常用分类 ID

| ID | 名称 |
|---|---|
| 6014 | Games |
| 6015 | Finance |
| 6016 | Entertainment |
| 6017 | Education |
| 6018 | Books |
| 6000 | All Categories (iOS) |
| 36 | Games (Android) |

## API 配额与缓存

- **月配额**：3,000 次/月（公司共用），仅 cache-miss 实际 API 调用计数
- **日限制**：全局 100 次/天、单人 30 次/天
- **缓存策略**：
  - 历史数据（end_date < 今天）：缓存 30 天（数据不可变）
  - 当天/近期数据：缓存 24 小时
  - 搜索/App 详情：缓存 7 天
- 返回数据中 `_cached: true` 表示命中缓存，`_api_calls_today` 表示今日已用 API 次数
- **优化建议**：尽量合并查询（多 app_id 逗号分隔），避免逐个查询浪费配额

## 认证流程

1. 用户从 Sensor Tower 后台生成 API Token
2. 运行 `python3 tools/st_query.py setup --token xxx`
3. Token 存入 `credentials.yaml` 的 `st.token` 字段
4. 后续查询通过 Bot 侧代理转发，Token 不进入容器环境
