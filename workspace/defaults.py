"""
项目常量定义
"""

BOT_NAME = "TyClaw"

# GA 平台配置（凭据在 works/{bucket}/{staff_id}/credentials.yaml）
GA_CONFIG = {
    "host": "giga-analytics-public.tuyoo.com",
    "port": 8810,
    "ssl": True,
}

# TD 投放数据平台配置（token 在 works/{bucket}/{staff_id}/credentials.yaml）
_TD_BASE = "https://pt09-tradedesk-online.tuyoo.com"
TD_CONFIG = {
    "report_url": f"{_TD_BASE}/api/ad-manager/report/v1/platform/",
    "hourly_url": f"{_TD_BASE}/api/ad-manager/platform/v1/hourly_data/data/",
    "channel_download_url": f"{_TD_BASE}/api/ad-manager/report/v1/platform/group/download/",
    "detail_download_url": f"{_TD_BASE}/api/ad-manager/report/v1/platformDetails/download/",
    "async_task_url": f"{_TD_BASE}/api/ad-manager/async/v1/tasks/",
    "dimension_fields": [
        "platform_id", "platform_name", "project_id", "project_name",
        "optimizer_id", "optimizer_name", "account_id", "account_name",
        "campaign_id", "campaign_name", "ad_id", "ad_name",
        "studio_id", "studio_name", "spread_type", "spread_type_display",
        "day", "month", "week",
    ],
}

# Web 角色定义
WEB_ROLES = {
    "admin": {
        "label": "管理员",
        "permissions": [
            "viewer", "private_viewer", "stats",
            "admin", "admin_web_users", "admin_users",
        ],
    },
    "developer": {
        "label": "开发者",
        "permissions": [
            "viewer", "private_viewer", "stats",
        ],
    },
    "member": {
        "label": "成员",
        "permissions": ["private_viewer"],
    },
    "guest": {
        "label": "访客",
        "permissions": [],
    },
}
