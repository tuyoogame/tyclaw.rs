# 可用指标字段 (column_list)

## 广告投放

| 字段名 | 中文名 | 说明 |
|--------|--------|------|
| cost | 消耗 | 广告消耗金额 |
| show | 展示 | 广告展示次数 |
| click | 点击 | 广告点击次数 |
| convert | 转化 | 转化次数 |
| active | 激活数 | 激活次数 |
| convert_cost | 转化成本 | 单次转化成本 |
| active_cost | 激活成本 | 单次激活成本 |
| cost_by_thousand_show | 千次展示成本 | CPM |
| ctr | 点击率 | 点击/展示 |
| cvr | 转化率 | 转化/点击 |
| ad_id_total | 广告ID总数 | 在投广告数 |
| ml_result | ML 结果 | 模型预测结果 |
| xgb_good | XGB 好 | XGB 模型好评数 |
| xgb_bad | XGB 差 | XGB 模型差评数 |

## 用户

| 字段名 | 中文名 | 说明 |
|--------|--------|------|
| new_user | 新增设备数 | 新增设备数 |
| new_paid_user | 新增付费用户 | 新增付费用户数 |
| new_paid_rate | 新增付费率 | 新增付费用户/新增用户 |
| new_user_cost | 新增成本 | 单个新增设备获取成本 |
| new_paid_user_cost | 新付费用户成本 | 单个新付费用户获取成本 |
| new_user_ad_trace | 再归因新增 | 再归因口径新增用户数 |
| advertiser_dau | DAU | 日活跃用户 |
| advertiser_paid_dau | 内购付费人数 | 日活跃内购付费用户 |

## 付费 (payN = 第N天累计付费)

| 字段名 | 中文名 |
|--------|--------|
| pay1 | 首日付费 |
| pay2 | 次日付费 |
| pay3 ~ pay15 | 3日~15日付费 |
| pay30 | 30日累计付费 |
| pay45 | 45日累计付费 |
| pay60 | 60日累计付费 |
| pay90 | 90日累计付费 |
| pay120 | 120日累计付费 |
| pay150 | 150日累计付费 |
| pay180 | 180日累计付费 |
| pay210 | 210日累计付费 |
| pay240 | 240日累计付费 |
| pay270 | 270日累计付费 |
| pay300 | 300日累计付费 |
| pay360 | 360日累计付费 |
| total_pay | 全生命周期累计付费 |
| pay1_ad_trace | 再归因首日付费 |
| pay7_ad_trace | 再归因7日付费 |
| pay3_1 | 3日单日付费 |
| pay7_1 | 7日单日付费 |
| arppu_1 | 首日ARPPU |
| advertiser_recharge | 内购流水 |
| income | 广告收入 |

完整 pay 字段列表: `pay1` `pay2` `pay3` `pay4` `pay5` `pay6` `pay7` `pay8` `pay9` `pay10` `pay11` `pay12` `pay13` `pay14` `pay15` `pay30` `pay45` `pay60` `pay90` `pay120` `pay150` `pay180` `pay210` `pay240` `pay270` `pay300` `pay360`

## LTV (ltvN = 第N天LTV)

| 字段名 | 中文名 |
|--------|--------|
| ltv1 | 首日LTV |
| ltv2 ~ ltv15 | 2日~15日LTV |
| ltv30 | 30日LTV |
| ltv45 | 45日LTV |
| ltv60 | 60日LTV |
| ltv90 | 90日LTV |
| ltv120 | 120日LTV |
| ltv150 | 150日LTV |
| ltv180 | 180日LTV |
| ltv210 | 210日LTV |
| ltv240 | 240日LTV |
| ltv270 | 270日LTV |
| ltv300 | 300日LTV |
| ltv360 | 360日LTV |
| total_ltv | 全生命周期LTV |

完整 ltv 字段列表: `ltv1` `ltv2` `ltv3` `ltv4` `ltv5` `ltv6` `ltv7` `ltv8` `ltv9` `ltv10` `ltv11` `ltv12` `ltv13` `ltv14` `ltv15` `ltv30` `ltv45` `ltv60` `ltv90` `ltv120` `ltv150` `ltv180` `ltv210` `ltv240` `ltv270` `ltv300` `ltv360`

## ROI (roiN = 第N天ROI)

| 字段名 | 中文名 |
|--------|--------|
| roi1 ~ roi7 | 第1~7日ROI |
| roi15 | 15日ROI |
| roi30 | 30日ROI |
| roi45 | 45日ROI |
| roi60 | 60日ROI |
| roi90 | 90日ROI |
| roi120 | 120日ROI |
| roi150 | 150日ROI |
| roi180 | 180日ROI |
| roi210 | 210日ROI |
| roi240 | 240日ROI |
| roi270 | 270日ROI |
| roi300 | 300日ROI |
| roi360 | 360日ROI |
| total_roi | 总ROI |

完整 roi 字段列表: `roi1` `roi2` `roi3` `roi4` `roi5` `roi6` `roi7` `roi15` `roi30` `roi45` `roi60` `roi90` `roi120` `roi150` `roi180` `roi210` `roi240` `roi270` `roi300` `roi360`

## 留存人数 (stay_numN = 第N日留存人数)

| 字段名 | 中文名 |
|--------|--------|
| stay_num2 ~ stay_num15 | 第2~15日留存人数 |
| stay_num30 | 30日留存人数 |
| stay_num45 | 45日留存人数 |
| stay_num60 | 60日留存人数 |
| stay_num90 | 90日留存人数 |
| stay_num120 | 120日留存人数 |
| stay_num150 | 150日留存人数 |
| stay_num180 | 180日留存人数 |

完整列表: `stay_num2` `stay_num3` `stay_num4` `stay_num5` `stay_num6` `stay_num7` `stay_num8` `stay_num9` `stay_num10` `stay_num11` `stay_num12` `stay_num13` `stay_num14` `stay_num15` `stay_num30` `stay_num45` `stay_num60` `stay_num90` `stay_num120` `stay_num150` `stay_num180`

## 留存率 (stayN = 第N日留存率)

| 字段名 | 中文名 |
|--------|--------|
| stay2 ~ stay15 | 第2~15日留存率 |
| stay30 | 30日留存率 |
| stay45 | 45日留存率 |
| stay60 | 60日留存率 |
| stay90 | 90日留存率 |
| stay120 | 120日留存率 |
| stay150 | 150日留存率 |
| stay180 | 180日留存率 |

完整列表: `stay2` `stay3` `stay4` `stay5` `stay6` `stay7` `stay8` `stay9` `stay10` `stay11` `stay12` `stay13` `stay14` `stay15` `stay30` `stay45` `stay60` `stay90` `stay120` `stay150` `stay180`

## 活跃天数 (timesN = 第N日活跃天数)

| 字段名 | 中文名 |
|--------|--------|
| times3 | 3日活跃天数 |
| times4 | 4日活跃天数 |
| times5 | 5日活跃天数 |
| times6 | 6日活跃天数 |
| times7 | 7日活跃天数 |

## 付费留存率

| 字段名 | 中文名 |
|--------|--------|
| pay2_stay_rate | 2日付费留存率 |
| pay3_stay_rate | 3日付费留存率 |
| pay7_stay_rate | 7日付费留存率 |

---

## 按场景推荐

### 投放概览
```json
["cost","show","click","convert","active","ctr","cvr","new_user","new_paid_user","pay1","roi1","roi7","total_roi","stay2"]
```

### 花费与效率
```json
["cost","show","click","ctr","cvr","convert_cost","active_cost","cost_by_thousand_show","new_user_cost","new_paid_user_cost"]
```

### ROI 分析
```json
["cost","pay1","roi1","roi3","roi7","roi15","roi30","roi60","roi90","roi180","roi360","total_roi"]
```

### 付费分析
```json
["new_paid_user","new_paid_rate","pay1","pay3","pay7","pay15","pay30","pay60","pay90","pay180","pay360","total_pay","arppu_1"]
```

### LTV 分析
```json
["new_user","ltv1","ltv3","ltv7","ltv15","ltv30","ltv60","ltv90","ltv180","ltv360","total_ltv"]
```

### 留存分析
```json
["new_user","stay2","stay3","stay7","stay15","stay30","stay60","stay90","stay180"]
```

### 用户获取
```json
["cost","new_user","new_user_ad_trace","new_user_cost","new_paid_user","new_paid_user_cost","stay2","stay7"]
```

---

## 自然语言关键词映射

| 用户关键词 | 推荐字段 |
|-----------|---------|
| 消耗/花费/cost | cost |
| 展示/曝光/impression | show |
| 点击 | click |
| 点击率/CTR | ctr |
| 转化率/CVR | cvr |
| 转化 | convert, convert_cost |
| 激活 | active, active_cost |
| CPM/千展 | cost_by_thousand_show |
| 新增/新增设备/新用户 | new_user, new_user_ad_trace |
| 再归因 | new_user_ad_trace, pay1_ad_trace, pay7_ad_trace |
| 付费/充值 | pay1, pay7, total_pay |
| 内购/内购流水 | advertiser_recharge |
| 付费率 | new_paid_rate |
| ROI/回报率 | roi1, roi7, total_roi |
| LTV | ltv1, ltv7, total_ltv |
| 留存/次留 | stay2, stay7, stay30 |
| 留存人数 | stay_num2, stay_num7 |
| 成本/CPA | new_user_cost, new_paid_user_cost, convert_cost |
| ARPPU | arppu_1 |
| DAU/日活 | advertiser_dau |
| 内购付费人数/付费DAU | advertiser_paid_dau |
| 付费留存 | pay2_stay_rate, pay7_stay_rate |
| 活跃天数 | times3, times7 |
| 广告收入/广告变现 | income |
