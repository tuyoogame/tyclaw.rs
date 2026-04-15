# API 端点定义

## 通用信息

- Base URL: `https://pt09-tradedesk-online.tuyoo.com/api/ad-manager/report/v1`
- 认证: `authorization: <JWT_TOKEN>` header
- Content-Type: `application/json`
- 方法: POST

---

## POST /platform/

报表平台维度查询。

### 请求体

```json
{
  "page": 1,
  "page_size": 20,
  "start_date": "2026-02-28",
  "end_date": "2026-02-28",
  "order_by": "cost",
  "order_type": "desc",
  "dimension": 5,
  "income_type": 2,
  "column_list": ["cost", "new_user", "roi1"],
  "project": ["2"],
  "platform": ["1"],
  "sub_channel": "4",
  "device_os": 1,
  "studio": ["1"],
  "spread_type": "1",
  "account_id_list": ["1233323"]
}
```

### 参数说明

#### 必填参数

| 参数 | 类型 | 必填 | 默认值 | 说明 |
|------|------|------|--------|------|
| page | number | 是 | 1 | 页码 |
| page_size | number | 是 | 20 | 每页条数 |
| start_date | string | 是 | - | 开始日期，格式 YYYY-MM-DD |
| end_date | string | 是 | - | 结束日期，格式 YYYY-MM-DD |
| order_by | string | 是 | "cost" | 排序字段，须为 column_list 中的字段 |
| order_type | string | 是 | "desc" | 排序方向：asc / desc |
| dimension | number | 是 | 5 | 查询维度（详见维度映射） |
| income_type | number | 是 | 1 | 收入类型（详见收入类型映射） |
| column_list | string[] | 是 | - | 需要查询的指标字段列表 |

#### 筛选参数（可选）

用户提到筛选条件时才加入，未提及则不传。

| 参数 | 类型 | 说明 |
|------|------|------|
| project | string[] | 项目 ID 列表，如 `["2"]` |
| platform | string[] | 平台 ID 列表，如 `["1"]` |
| sub_channel | string | 子渠道 ID，如 `"4"` |
| device_os | number | 设备系统，如 `1` |
| studio | string[] | 工作室 ID 列表，如 `["1"]` |
| spread_type | string | 推广类型，如 `"1"` |
| account_id_list | string[] | 广告账户 ID 列表，如 `["1233323"]` |
| strategy | number | 投放策略，1=手动投放，2=自动投放 |

### 维度 (dimension) 映射

| 值 | 含义 |
|----|------|
| 1 | 日 |
| 2 | 月 |
| 3 | 投放应用 |
| 4 | 子平台 |
| 5 | 渠道 |
| 6 | 计划名 |
| 7 | 计划ID |
| 8 | 账户名称 |
| 9 | 账户ID |
| 10 | 广告名称 |
| 11 | 广告ID |
| 12 | 营销工作室 |
| 13 | 优化师 |
| 14 | 二级渠道 |
| 15 | 推广类型 |
| 16 | 账户备注 |
| 17 | 代理商 |
| 18 | 营销组 |
| 19 | 投放策略 |
| 20 | 开户主体 |
| 21 | 周 |

### 收入类型 (income_type) 映射

| 值 | 含义 |
|----|------|
| 1 | 净收入 |
| 2 | 账面收入（默认） |

### curl 示例

基础查询（无筛选）：

```bash
curl --location 'https://pt09-tradedesk-online.tuyoo.com/api/ad-manager/report/v1/platform/' \
--header 'authorization: YOUR_TOKEN' \
--header 'Content-Type: application/json' \
--data '{
    "page": 1,
    "page_size": 20,
    "start_date": "2026-02-28",
    "end_date": "2026-02-28",
    "order_by": "cost",
    "order_type": "desc",
    "dimension": 5,
    "income_type": 2,
    "column_list": ["cost","new_user","roi1"]
}'
```

带筛选条件：

```bash
curl --location 'https://pt09-tradedesk-online.tuyoo.com/api/ad-manager/report/v1/platform/' \
--header 'authorization: YOUR_TOKEN' \
--header 'Content-Type: application/json' \
--data '{
    "page": 1,
    "page_size": 20,
    "start_date": "2026-02-28",
    "end_date": "2026-02-28",
    "order_by": "cost",
    "order_type": "desc",
    "dimension": 5,
    "income_type": 2,
    "column_list": ["cost","new_user","roi1"],
    "project": ["2"],
    "platform": ["1"],
    "sub_channel": "4",
    "device_os": 1,
    "studio": ["1"],
    "spread_type": "1",
    "account_id_list": ["1233323"]
}'
```

### 响应结构

```json
{
  "code": 0,
  "data": {
    "data_list": [ ... ],
    "page": 1,
    "page_size": 20,
    "total": 4,
    "summary": { ... },
    "last_time": "1772262467",
    "day_last_time": "1772260867"
  },
  "detail": "成功"
}
```

| 字段 | 类型 | 说明 |
|------|------|------|
| code | number | 0=成功，非0=失败 |
| detail | string | 状态描述 |
| data.data_list | array | 数据行列表 |
| data.page | number | 当前页码 |
| data.page_size | number | 每页条数 |
| data.total | number | 总条数 |
| data.summary | object | 汇总行（所有行的聚合值） |

每行数据包含：
- **维度字段**: `platform_id`, `platform_name`, `project_id`, `optimizer_id`, `device_os` 等（根据 dimension 不同返回不同维度字段）
- **指标字段**: 与请求中 `column_list` 对应的指标值

---

## POST /api/ad-manager/platform/v1/hourly_data/data/

分小时报表查询，返回指定日期每小时的投放数据明细。

> **注意：** 此端点 Base URL 与常规报表不同，完整地址为 `https://pt09-tradedesk-online.tuyoo.com/api/ad-manager/platform/v1/hourly_data/data/`

### 请求体

```json
{
  "date": "2026-03-02",
  "account_id": "1111333333",
  "platform": 1,
  "sub_channel": 4,
  "project_id": 2,
  "device_os": 1,
  "studio_id": 1,
  "spread_type": "1"
}
```

### 参数说明

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| date | string | 是 | 查询日期，格式 YYYY-MM-DD，仅支持单日 |
| project_id | number | 否 | 项目 ID（注意：不是数组，是单个数值） |
| platform | number | 否 | 平台 ID（注意：不是数组，是单个数值） |
| sub_channel | number | 否 | 子渠道 ID |
| device_os | number | 否 | 设备系统 |
| studio_id | number | 否 | 工作室 ID（注意：不是数组，是单个数值） |
| spread_type | string | 否 | 推广类型 |
| account_id | string | 否 | 单个广告账户 ID |

> **与 /platform/ 端点的参数差异：**
> - `date` 单日 vs `start_date`/`end_date` 日期区间
> - `project_id`(number) vs `project`(string[])
> - `platform`(number) vs `platform`(string[])
> - `studio_id`(number) vs `studio`(string[])
> - `account_id`(string) vs `account_id_list`(string[])
> - 无 `page`/`page_size`/`order_by`/`order_type`/`dimension`/`income_type`/`column_list`

### 响应结构

```json
{
  "code": 0,
  "data": {
    "data_list": [
      {
        "date": "2026-03-02",
        "hour": "14:00",
        "new_user": 69,
        "pay_user": 4,
        "pay1": 412,
        "cost": 23363.62,
        "show": 560015,
        "click": 4546,
        "active": 71,
        "cost_per_new_user": 338.60318
      }
    ],
    "new_user_total": 770,
    "pay_user_total": 29,
    "pay1_total": 1442,
    "account_total": 29,
    "cost_total": 268124.66,
    "show_total": 6750988,
    "click_total": 56892,
    "active_total": 736,
    "cost_per_new_user_total": 348.21384,
    "latest_time": "2026-03-02 15:08"
  },
  "detail": "成功"
}
```

#### data_list 每行字段

| 字段 | 类型 | 说明 |
|------|------|------|
| date | string | 日期 |
| hour | string | 小时，如 "14:00" |
| new_user | number | 新增设备数 |
| pay_user | number | 付费用户数（可能缺省） |
| pay1 | number | 首日付费（可能缺省） |
| cost | number | 消耗 |
| show | number | 展示 |
| click | number | 点击 |
| active | number | 激活 |
| cost_per_new_user | number | 新增成本 |

#### 汇总字段（data 顶层）

| 字段 | 说明 |
|------|------|
| new_user_total | 新增设备汇总 |
| pay_user_total | 付费用户汇总 |
| pay1_total | 首日付费汇总 |
| account_total | 账户数 |
| cost_total | 消耗汇总 |
| show_total | 展示汇总 |
| click_total | 点击汇总 |
| active_total | 激活汇总 |
| cost_per_new_user_total | 平均新增成本 |
| latest_time | 数据最新更新时间 |

### curl 示例

```bash
curl --location 'https://pt09-tradedesk-online.tuyoo.com/api/ad-manager/platform/v1/hourly_data/data/' \
--header 'authorization: YOUR_TOKEN' \
--header 'Content-Type: application/json' \
--data '{
    "date": "2026-03-02",
    "project_id": 2,
    "platform": 1,
    "sub_channel": 4,
    "device_os": 1
}'
```

---

## POST /api/ad-manager/report/v1/platform/group/download/

渠道报表聚合下载。返回 Excel 文件流（小日期范围）或异步任务 ID（大日期范围）。

### 请求体

```json
{
  "dimension_list": [5, 3],
  "start_date": "2026-03-19",
  "end_date": "2026-03-19",
  "order_by": "cost",
  "order_type": "desc",
  "column_list": ["cost", "new_user", "roi1"],
  "project": ["6997"],
  "platform": ["1"],
  "income_type": 2
}
```

### 参数说明

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| dimension_list | number[] | 是 | 聚合维度列表（枚举值同 /platform/ 的 dimension） |
| start_date | string | 是 | 开始日期 YYYY-MM-DD |
| end_date | string | 是 | 结束日期 YYYY-MM-DD |
| order_by | string | 否 | 排序字段，默认 cost |
| order_type | string | 否 | asc / desc |
| column_list | string[] | 否 | 指标字段列表 |

筛选参数与 /platform/ 完全相同（project, platform, sub_channel, device_os, studio, spread_type, account_id_list, strategy, optimizer, income_type）。

### 响应

- 小日期范围：直接返回 `application/octet-stream`（Excel 文件）
- 大日期范围：返回 `{"code": 102241, "data": {"id": <task_id>}}`，需要轮询异步任务

---

## POST /api/ad-manager/report/v1/platformDetails/download/

渠道明细报表下载。支持优化目标、素材类型等在线报表不支持的维度。

### 请求体

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
  "column_list": ["cost", "new_user", "roi1", "optimization_goal"],
  "project": ["6997"]
}
```

### 参数说明

#### 必填参数

| 参数 | 类型 | 必填 | 默认值 | 说明 |
|------|------|------|--------|------|
| dimension | number | 是 | 1 | 主维度（dime_pm_* 枚举） |
| start_date | string | 是 | - | 开始日期 YYYY-MM-DD |
| end_date | string | 是 | - | 结束日期 YYYY-MM-DD |
| is_download | bool | 是 | true | 固定为 true |
| is_detail | bool | 是 | false | true=单条素材明细，false=聚合模式 |

#### 聚合参数

| 参数 | 类型 | 说明 |
|------|------|------|
| dimension_items | number[] | 聚合维度列表（dime_pm_* 枚举） |
| page | number | 页码，默认 1 |
| page_size | number | 每页条数，默认 100 |
| order_by | string | 排序字段 |
| order_type | string | asc / desc |
| column_list | string[] | 指标字段列表 |

#### 筛选参数

与 /platform/ 基本相同，加上明细特有参数：

| 参数 | 类型 | 说明 |
|------|------|------|
| project | string[] | 项目 ID 列表 |
| platform | string[] | 平台 ID 列表 |
| optimizer | string[] | 优化师 ID 列表 |
| studio | string[] | 工作室 ID 列表 |
| sub_channel | uint32 | 子渠道（注意：number 不是 string） |
| spread_type | string | 推广类型 |
| device_os | uint32 | 设备系统 |
| strategy | uint32 | 投放策略 |
| income_type | uint32 | 收入类型 |
| account_id_list | string[] | 账户 ID 列表 |

### dimension / dimension_items 枚举 (dime_pm_*)

| 值 | 说明 |
|----|------|
| 1 | 日 |
| 2 | 月 |
| 3 | 渠道 |
| 4 | 二级渠道 |
| 5 | 营销工作室 |
| 6 | 优化师 |
| 7 | 投放应用 |
| 8 | 推广类型 |
| 9 | 子平台 |
| 10 | 账户ID |
| 11 | 账户名称 |
| 12 | 计划ID |
| 13 | 计划名 |
| 14 | 广告ID |
| 15 | 广告名称 |
| 16 | 优化目标 |
| 17 | 投放版位 |
| 18 | 素材类型 |
| 19 | 素材md5 |
| 22 | 营销组 |
| 23 | 投放策略 |
| 24 | 开户主体 |
| 25 | 备注 |
| 27 | 周 |

> **注意：** 此维度枚举（dime_pm_*）与 /platform/ 的 dimension 编号含义不同！例如 3=渠道（这里）vs 3=投放应用（/platform/）。

### 响应

同渠道报表下载：直接返回 Excel 或异步任务 ID。

---

## GET /api/ad-manager/async/v1/tasks/{id}/

异步任务状态查询。

### 响应

```json
{
  "code": 0,
  "data": {
    "url": "https://...obs.../report_xxx.xlsx"
  }
}
```

| code | 含义 |
|------|------|
| 0 | 完成，data.url 为下载链接 |
| 102244 | 任务失败 |
| 其他 | 仍在处理中 |
