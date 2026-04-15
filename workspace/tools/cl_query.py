#!/usr/bin/env python3
"""创量智投 (Chuangliang) API Client — 账户管理、素材查询、巨量广告投放"""

import argparse
import hashlib
import json
import os
import sys
import time
from datetime import datetime, timedelta
from pathlib import Path

import requests

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
from utils import (get_injected_credential, save_user_credentials, clear_user_credentials,
                   sync_credential_env, clear_credential_env)

API_BASE = "https://cli1.mobgi.com"
ORIGIN = "https://cl.mobgi.com"

_DAY_NAMES = {"mon": 0, "tue": 1, "wed": 2, "thu": 3,
              "fri": 4, "sat": 5, "sun": 6}


def build_schedule_time(spec: str) -> str:
    """人类可读时段 → 336 位 0/1 字符串（7天×48个半小时）。
    格式: day_range:HH:MM-HH:MM[,day_range:HH:MM-HH:MM,...]
    day_range: mon/tue/.../sun, mon-fri 范围, 或 all
    时间精度: 30 分钟（只接受 :00 / :30）
    示例: "tue-thu:01:00-18:30,fri:09:00-12:00"
    """
    bits = ['0'] * 336
    for part in spec.split(","):
        part = part.strip()
        day_str, time_str = part.split(":", 1)
        if day_str == "all":
            days = list(range(7))
        elif "-" in day_str:
            d1, d2 = day_str.split("-")
            days = list(range(_DAY_NAMES[d1.strip().lower()],
                              _DAY_NAMES[d2.strip().lower()] + 1))
        else:
            days = [_DAY_NAMES[day_str.strip().lower()]]
        t_start, t_end = time_str.split("-")
        sh, sm = map(int, t_start.split(":"))
        eh, em = map(int, t_end.split(":"))
        slot_start = sh * 2 + sm // 30
        slot_end = eh * 2 + (1 if em > 0 else 0)
        for day in days:
            for s in range(slot_start, slot_end):
                bits[day * 48 + s] = '1'
    return ''.join(bits)
def _session_cache_path(name: str) -> Path:
    personal = os.environ.get("TYCLAW_PERSONAL_DIR", "")
    if personal:
        return Path(personal) / ".cache" / name
    return Path.home() / ".cache" / name

SESSION_CACHE = _session_cache_path("cl_session.json")

MEDIA_TYPES = {
    "toutiao": "巨量广告",
    "kuaishou": "磁力智投",
    "gdt": "腾讯广告",
    "baidu": "百度信息流",
    "baidu_search": "百度搜索",
    "bilibili": "B站营销",
    "jinniu": "磁力金牛",
    "baidushop": "百度电商",
    "qianchuan_universe": "巨量千川(全域)",
    "localads": "巨量本地推",
    "redbook": "小红书",
}


def md5(text: str) -> str:
    return hashlib.md5(text.encode()).hexdigest()


class CLClient:
    """创量智投 API 客户端"""

    def __init__(self, email: str, password: str):
        self.email = email
        self.password_md5 = md5(password)
        self.session = requests.Session()
        self.session.headers.update({
            "User-Agent": "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) "
                          "AppleWebKit/537.36 (KHTML, like Gecko) "
                          "Chrome/120.0.0.0 Safari/537.36",
            "Origin": ORIGIN,
            "Referer": f"{ORIGIN}/",
            "Accept": "application/json, text/plain, */*",
        })
        self.user_id = None
        self.main_user_id = None
        self.user_info = None
        self._load_session()

    # ── session 持久化 ──

    def _save_session(self):
        SESSION_CACHE.parent.mkdir(parents=True, exist_ok=True)
        data = {
            "email": self.email,
            "cookies": self.session.cookies.get_dict(),
            "user_id": self.user_id,
            "main_user_id": self.main_user_id,
            "ts": time.time(),
        }
        SESSION_CACHE.write_text(json.dumps(data))

    def _load_session(self):
        if not SESSION_CACHE.exists():
            return
        try:
            data = json.loads(SESSION_CACHE.read_text())
            if data.get("email") != self.email:
                return
            if time.time() - data.get("ts", 0) > 7200:
                return
            for k, v in data["cookies"].items():
                self.session.cookies.set(k, v, domain="mobgi.com")
            self.user_id = data.get("user_id")
            self.main_user_id = data.get("main_user_id")
        except Exception:
            pass

    # ── 登录 ──

    def login(self) -> bool:
        if self.user_id and self._test_session():
            print("[login] session still valid, skipping login", file=sys.stderr)
            return True

        resp = self._raw_post("/User/AdminUser/loginInfo", {
            "email": self.email,
            "password": self.password_md5,
            "login_origin": "web",
        })
        if resp.get("code") != 0:
            print(f"[login] pre-login failed: {resp.get('message')}",
                  file=sys.stderr)
            return False

        pvd = resp.get("data", {}).get("product_version_data", [])
        pv = pvd[0]["product_version"] if pvd else 0

        resp = self._raw_post("/User/AdminUser/login", {
            "email": self.email,
            "password": self.password_md5,
            "product_version": pv,
            "login_origin": "web",
        })
        if resp.get("code") != 0:
            print(f"[login] login failed: {resp.get('message')}",
                  file=sys.stderr)
            return False

        login_user = resp.get("data", {}).get("login_user", {})
        self.user_id = login_user.get("user_id")
        self.main_user_id = login_user.get("main_user_id")
        self.user_info = login_user
        self._save_session()
        print(f"[login] success as {login_user.get('user_name', '')}",
              file=sys.stderr)
        return True

    def _test_session(self) -> bool:
        try:
            resp = self._raw_post("/User/AdminUser/getAdminSetting", {})
            return resp.get("code") == 0
        except Exception:
            return False

    def _raw_post(self, path: str, payload: dict,
                  timeout: int = 60) -> dict:
        resp = self.session.post(
            f"{API_BASE}{path}",
            json=payload,
            headers={"Content-Type": "application/json;charset=UTF-8"},
            timeout=timeout,
        )
        return resp.json()

    def _post(self, path: str, payload: dict,
              timeout: int = 60) -> dict:
        resp = self._raw_post(path, payload, timeout=timeout)
        if resp.get("code") in (401, -401):
            print("[api] session expired, re-logging in...", file=sys.stderr)
            SESSION_CACHE.unlink(missing_ok=True)
            self.user_id = None
            if self.login():
                resp = self._raw_post(path, payload, timeout=timeout)
        return resp

    def _biz_post(self, path: str, payload: dict,
                  timeout: int = 60) -> dict:
        """带业务 headers (client-user / main-user-id) 的 POST"""
        extra = {
            "Content-Type": "application/json;charset=UTF-8",
            "client-user": str(self.user_id or ""),
            "main-user-id": str(self.main_user_id or ""),
        }
        resp = self.session.post(
            f"{API_BASE}{path}", json=payload,
            headers=extra, timeout=timeout,
        )
        result = resp.json()
        if result.get("code") in (401, -401):
            print("[api] session expired, re-logging in...", file=sys.stderr)
            SESSION_CACHE.unlink(missing_ok=True)
            self.user_id = None
            if self.login():
                extra["client-user"] = str(self.user_id or "")
                extra["main-user-id"] = str(self.main_user_id or "")
                resp = self.session.post(
                    f"{API_BASE}{path}", json=payload,
                    headers=extra, timeout=timeout,
                )
                result = resp.json()
        return result

    # ── 账户相关 ──

    def get_media_accounts(self, media_type: str = "all",
                           page: int = 1, page_size: int = 20,
                           **filters) -> dict:
        return self._post("/Media/Account/getList", {
            "media_type": media_type,
            "page": page,
            "page_size": page_size,
            **filters,
        })

    def get_my_menu(self) -> dict:
        return self._post("/User/AdminUser/getMyMenu", {})

    def get_my_users(self) -> dict:
        return self._post("/User/AdminUser/getMyOptimizeUsers", {})

    # ── 素材相关 ──

    def get_material_list(self, page: int = 1, page_size: int = 20,
                          group_id: int = 0, material_type: str = "",
                          **filters) -> dict:
        payload = {
            "page": page,
            "page_size": page_size,
            **filters,
        }
        if group_id:
            payload["group_id"] = group_id
        if material_type:
            payload["material_type"] = material_type
        return self._post("/Material/Manage/lists", payload)

    def search_material(self, keyword: str, page: int = 1,
                        page_size: int = 20) -> dict:
        return self._post("/Material/Manage/lists", {
            "search_keyword": keyword,
            "page": page,
            "page_size": page_size,
        })

    # ── 账户报表 ──

    def get_account_report(self, start_date: str, end_date: str,
                           media_type: str = "toutiao_upgrade",
                           keyword: str = "",
                           search_field: str = "advertiser_id",
                           page: int = 1, page_size: int = 20) -> dict:
        """账户级别报表（含预算、余额、消耗、ROI 等）"""
        is_toutiao = media_type.startswith("toutiao")
        cost_field = "stat_cost" if is_toutiao else "cost"

        if is_toutiao:
            conditions: dict = {
                "owner_user_id": [], "cl_app_id": [],
                "company": [], "media_project_id": [],
                "admin_project_id": "",
                "auto_fix_reject_material_status": "",
                "search_keyword": keyword,
                "search_field": search_field if keyword else "",
            }
            base_infos = [
                "advertiser_nick", "advertiser_id", "user_name",
                "advertiser_status", "balance", "budget", "note",
            ]
            kpis = [
                cost_field,
                "attribution_game_in_app_ltv_1day",
                "attribution_game_in_app_roi_1day",
                "attribution_game_in_app_roi_8days",
                "active_pay_intra_day_count",
                "active_pay_intra_day_cost",
                "show_cnt", "click_cnt", "ctr",
                "active", "active_cost",
                "active_register", "active_register_cost",
                "attribution_day_active_pay_count",
                "attribution_game_pay_7d_count",
            ]
        else:
            conditions = {
                "owner_user_id": [], "company": [],
                "media_project_id": [], "balance": "",
                "admin_project_id": "",
                "search_keyword": keyword,
                "search_field": search_field if keyword else "",
                "time_line": "ACTIVE_TIME",
            }
            base_infos = [
                "advertiser_id", "advertiser_nick", "note",
                "balance", "daily_budget",
            ]
            kpis = [
                cost_field,
                "roi_activated_d1", "first_day_pay_amount",
                "first_day_first_pay_count",
                "payment_cost_activated_d1",
                "roi_activated_d3", "roi_activated_d7",
            ]

        payload: dict = {
            "data_type": "list",
            "media_type": media_type,
            "conditions": conditions,
            "sort_field": cost_field,
            "sort_direction": "desc",
            "base_infos": base_infos,
            "page": page,
            "page_size": page_size,
            "start_date": start_date,
            "end_date": end_date,
            "kpis": kpis,
        }
        if not is_toutiao:
            payload["time_line"] = "ACTIVE_TIME"
        return self._biz_post(
            "/MainPanelReport/AccountReport/getReport", payload)

    # ── 素材报表 ──

    def get_material_report(self, start_date: str, end_date: str,
                            media_type: str = "aggregate",
                            keyword: str = "",
                            page: int = 1, page_size: int = 20,
                            kpis: list[str] | None = None) -> dict:
        """素材报表查询。media_type: toutiao_upgrade / gdt_upgrade / aggregate"""
        is_toutiao = media_type.startswith("toutiao")
        cost_field = "stat_cost" if is_toutiao else "cost"
        default_kpis = [cost_field]

        conditions: dict = {
            "search_type": "name",
            "media_project_id": [],
            "material_special_id": [],
            "advertiser_id": [],
            "owner_user_id": [],
            "media_advertiser_company": [],
            "material_type": "",
            "label_ids": [],
            "material_origin_type": [],
            "customer_user_id1": [],
            "customer_user_id2": [],
            "material_group_id": [],
        }
        if keyword:
            conditions["keyword"] = keyword
        if not is_toutiao:
            conditions["time_line"] = "REPORTING_TIME"

        return self._biz_post("/ReportV23/MaterialReport/getReport", {
            "time_dim": "sum",
            "media_type": media_type,
            "data_type": "list",
            "data_dim": "material",
            "conditions": conditions,
            "sort_field": cost_field,
            "sort_direction": "desc",
            "kpis": kpis or default_kpis,
            "relate_dims": [],
            "start_date": start_date,
            "end_date": end_date,
            "db_type": "doris",
            "page": page,
            "page_size": page_size,
        })

    # ── 巨量广告投放 ──

    @staticmethod
    def _build_conditions(start_date: str, end_date: str,
                          keyword: str = "", status: str = "") -> str:
        """构建投放列表 conditions JSON 字符串"""
        conds = {
            "search_field": "name",
            "search_keyword": keyword,
            "search_type": "like",
            "cl_project_id": [],
            "cl_app_id": [],
            "user_id": [],
            "external_action": [],
            "deep_external_action": [],
            "deep_bid_type": [],
            "companys": [],
            "media_account_id": [],
            "landing_type": "",
            "delivery_mode": "",
            "status_first": "",
            "ad_type": "",
            "status": status,
            "status_second": "",
            "material_id": [],
            "cdt_start_date": f"{start_date} 00:00:00",
            "cdt_end_date": f"{end_date} 23:59:59",
        }
        return json.dumps(conds, ensure_ascii=False)

    def get_project_list(self, start_date: str, end_date: str,
                         page: int = 1, page_size: int = 20,
                         keyword: str = "", status: str = "") -> dict:
        return self._biz_post("/Toutiao/Project/getList", {
            "conditions": self._build_conditions(
                start_date, end_date, keyword, status),
            "start_date": start_date,
            "end_date": end_date,
            "page": page,
            "page_size": page_size,
            "sort_field": "project_create_time",
            "sort_direction": "desc",
            "data_type": "list",
            "select_kpi_fields": [
                "budget", "project_create_time", "stat_cost",
                "show_cnt", "cpa_bid", "deep_cpabid",
                "attribution_game_in_app_ltv_1day",
                "attribution_game_in_app_roi_1day",
                "attribution_day_active_pay_count",
            ],
        })

    def get_gdt_ad_list(self, start_date: str, end_date: str,
                     page: int, page_size: int) -> dict:
        return self._biz_post("/MainPanelReport/AdReport/getReport", {
            "data_type": "list",
            "media_type": "gdt_upgrade",
            "conditions": {
                "company": [], "owner_user_id": [],
                "advertiser_id": [], "media_project_id": [],
                "configured_status": "", "system_status": [],
                "created_time": [start_date, end_date],
                "last_modified_time": [],
                "combinatorial_id": "",
                "time_line": "REPORTING_TIME",
            },
            "sort_field": "created_time",
            "sort_direction": "desc",
            "base_infos": [
                "adgroup_name", "adgroup_id", "advertiser_id",
                "balance", "deep_bid_amount", "optimization_goal",
                "bid_amount", "created_time", "daily_budget",
                "bid_mode", "begin_date",
            ],
            "kpis": [
                "cost", "view_count", "ctr",
                "conversions_count", "conversions_cost",
                "deep_conversions_count", "deep_conversions_cost",
                "first_day_pay_amount", "roi_activated_d1",
            ],
            "page": page,
            "page_size": page_size,
            "start_date": start_date,
            "end_date": end_date,
            "time_line": "REPORTING_TIME",
        })

    def update_project_status(self, project_ids: list[str],
                              status: str) -> dict:
        """变更项目状态。status: ENABLE / DISABLE（必须大写）"""
        return self._biz_post("/Toutiao/Project/updateStatus", {
            "project_ids": project_ids,
            "opt_status": status.upper(),
        })

    def update_project_schedule(self, project_ids: list[str],
                                media_account_ids: list[int],
                                schedule_time: str) -> dict:
        """批量修改巨量广告项目投放时段。
        schedule_time: 336 位 0/1 字符串，全 0 表示"不限"。
        """
        data = [
            {"project_id": pid, "media_account_id": mid,
             "schedule_time": schedule_time}
            for pid, mid in zip(project_ids, media_account_ids)
        ]
        return self._biz_post("/Toutiao/Project/updateProjectField", {
            "data": data,
            "field": "schedule_time",
        })

    def update_toutiao_budget(self, media_account_ids: list[str],
                              budgets: list[str],
                              budget_mode: str = "BUDGET_MODE_DAY") -> dict:
        """批量修改巨量广告账户预算。budget_mode: BUDGET_MODE_DAY / BUDGET_MODE_TOTAL"""
        data = [
            {"media_account_id": mid, "budget_mode": budget_mode,
             "budget": b}
            for mid, b in zip(media_account_ids, budgets)
        ]
        return self._biz_post("/Toutiao/Advertiser/updateBudgetBatch", {
            "data": data,
            "time": "now",
        })

    def update_gdt_budget(self, media_account_ids: list[str],
                          daily_budgets: list[int]) -> dict:
        """批量修改腾讯广告账户日预算"""
        data = [
            {"media_account_id": mid, "daily_budget": b}
            for mid, b in zip(media_account_ids, daily_budgets)
        ]
        return self._biz_post("/Gdt/MainList/updateBudgetBatchV1", {
            "data": data,
            "time": "now",
        })

    def update_gdt_ad_dates(self, adgroup_ids: list[str],
                            advertiser_ids: list[str],
                            begin_date: str,
                            end_date: str) -> dict:
        """批量修改腾讯广告投放日期"""
        value = [{"begin_date": begin_date, "end_date": end_date}
                 for _ in adgroup_ids]
        return self._biz_post("/Gdt/MainList/updateAdGroupBatchV1", {
            "advertiser_ids": advertiser_ids,
            "adgroup_ids": adgroup_ids,
            "value": value,
            "time": "now",
            "field": "date",
        })

    # ── 任务中心 ──

    _MISSION_OPT_TYPES = [
        "update_schedule_time", "update_bid", "update_deep_cpabid",
        "update_roi", "update_plan_budget", "update_date",
        "update_daily_budget", "update_time_series",
        "update_play_material", "advertising",
        "enable", "delete", "disable",
    ]

    def get_processing_missions(self,
                                opt_types: list[str] | None = None) -> dict:
        extra = {
            "Content-Type": "application/json;charset=UTF-8",
            "client-user": str(self.user_id or ""),
            "main-user-id": str(self.main_user_id or ""),
        }
        params = {"opt_type[]": opt_types or self._MISSION_OPT_TYPES}
        resp = self.session.get(
            f"{API_BASE}/Basic/MissionCenter/getProcessingMissions",
            params=params, headers=extra, timeout=30,
        )
        return resp.json()

    def wait_missions_done(self, opt_types: list[str] | None = None,
                           timeout: int = 60, interval: float = 2) -> bool:
        """轮询直到相关任务全部完成，返回是否在超时前完成"""
        import time as _t
        deadline = _t.time() + timeout
        while _t.time() < deadline:
            r = self.get_processing_missions(opt_types)
            tasks = r.get("data", [])
            if not tasks:
                return True
            names = [t.get("mission_name", "") for t in tasks]
            print(f"[mission] waiting: {', '.join(names)}", file=sys.stderr)
            _t.sleep(interval)
        return False

    # ── 站内信 ──

    def get_messages(self, page: int = 1, page_size: int = 10) -> dict:
        return self._post("/StationLetter/Message/getList", {
            "page": page,
            "page_size": page_size,
        })


def _cmd_setup(args):
    staff_id = os.environ.get("TYCLAW_SENDER_STAFF_ID", "")
    if not staff_id:
        print("Error: TYCLAW_SENDER_STAFF_ID env var required", file=sys.stderr)
        sys.exit(1)
    data = {"email": args.setup_email, "password": args.setup_password}
    save_user_credentials(staff_id, "cl", data)
    sync_credential_env("cl", data)
    print(f"CL credentials saved for {staff_id}")


def _cmd_clear_credentials(_args):
    staff_id = os.environ.get("TYCLAW_SENDER_STAFF_ID", "")
    if not staff_id:
        print("Error: TYCLAW_SENDER_STAFF_ID env var required", file=sys.stderr)
        sys.exit(1)
    if clear_user_credentials(staff_id, "cl"):
        clear_credential_env("cl")
        print(f"CL credentials cleared for {staff_id}")
    else:
        print(f"No CL credentials found for {staff_id}")


def main():
    parser = argparse.ArgumentParser(description="创量智投 CLI")
    parser.add_argument("--email", default="")
    parser.add_argument("--password", default="")
    sub = parser.add_subparsers(dest="action", required=True)

    p_setup = sub.add_parser("setup", help="Set CL credentials")
    p_setup.add_argument("--email", required=True, dest="setup_email")
    p_setup.add_argument("--password", required=True, dest="setup_password")
    sub.add_parser("clear-credentials", help="Clear CL credentials")

    p_accts = sub.add_parser("accounts", help="媒体账户列表")
    p_accts.add_argument("--media-type", default="all")
    p_accts.add_argument("--page", type=int, default=1)
    p_accts.add_argument("--page-size", type=int, default=20)

    sub.add_parser("menu", help="获取当前用户菜单")

    sub.add_parser("users", help="获取优化师列表")

    sub.add_parser("media_types", help="列出支持的媒体类型")

    p_mlist = sub.add_parser("materials", help="素材列表")
    p_mlist.add_argument("--page", type=int, default=1)
    p_mlist.add_argument("--page-size", type=int, default=20)
    p_mlist.add_argument("--group-id", type=int, default=0)
    p_mlist.add_argument("--type", default="", dest="material_type")

    p_msearch = sub.add_parser("material_search", help="搜索素材")
    p_msearch.add_argument("--keyword", required=True)
    p_msearch.add_argument("--page", type=int, default=1)
    p_msearch.add_argument("--page-size", type=int, default=20)

    p_arpt = sub.add_parser("account_report", help="账户报表（含预算/余额/消耗/ROI）")
    p_arpt.add_argument("--media-type", default="toutiao_upgrade",
                        help="toutiao_upgrade / gdt_upgrade")
    p_arpt.add_argument("--keyword", default="",
                        help="搜索关键词（配合 --search-field 使用）")
    p_arpt.add_argument("--search-field", default="advertiser_id",
                        help="搜索字段：advertiser_id / advertiser_nick")
    p_arpt.add_argument("--days", type=int, default=1)
    p_arpt.add_argument("--start-date")
    p_arpt.add_argument("--end-date")
    p_arpt.add_argument("--page", type=int, default=1)
    p_arpt.add_argument("--page-size", type=int, default=20)

    p_mrpt = sub.add_parser("material_report", help="素材报表")
    p_mrpt.add_argument("--media-type", default="aggregate",
                        help="toutiao_upgrade / gdt_upgrade / aggregate（默认汇总）")
    p_mrpt.add_argument("--keyword", default="", help="素材名称关键词")
    p_mrpt.add_argument("--days", type=int, default=7)
    p_mrpt.add_argument("--start-date")
    p_mrpt.add_argument("--end-date")
    p_mrpt.add_argument("--page", type=int, default=1)
    p_mrpt.add_argument("--page-size", type=int, default=20)

    p_projects = sub.add_parser("projects", help="巨量广告项目列表")
    p_projects.add_argument("--days", type=int, default=7)
    p_projects.add_argument("--start-date")
    p_projects.add_argument("--end-date")
    p_projects.add_argument("--page", type=int, default=1)
    p_projects.add_argument("--page-size", type=int, default=20)
    p_projects.add_argument("--keyword", default="")
    p_projects.add_argument("--status", default="",
                            help="enable/disable 或留空查全部")

    p_ads = sub.add_parser("gdt_ads", help="腾讯广告列表")
    p_ads.add_argument("--days", type=int, default=7)
    p_ads.add_argument("--start-date")
    p_ads.add_argument("--end-date")
    p_ads.add_argument("--page", type=int, default=1)
    p_ads.add_argument("--page-size", type=int, default=20)

    p_status = sub.add_parser("update_status", help="巨量广告项目状态变更")
    p_status.add_argument("--ids", required=True,
                          help="项目 ID（逗号分隔）")
    p_status.add_argument("--status", required=True,
                          choices=["enable", "disable"],
                          help="enable=启用, disable=暂停")

    p_sched = sub.add_parser("toutiao_update_project_schedule",
                              help="批量修改巨量广告项目投放时段")
    p_sched.add_argument("--project-ids", required=True,
                         help="项目 ID（逗号分隔）")
    p_sched.add_argument("--media-account-ids", required=True,
                         help="媒体账户 ID（逗号分隔，与 project-ids 一一对应）")
    p_sched_g = p_sched.add_mutually_exclusive_group(required=True)
    p_sched_g.add_argument(
        "--schedule",
        help="人类可读时段，如 tue-thu:01:00-18:30（留空用 --no-limit）")
    p_sched_g.add_argument(
        "--schedule-raw",
        help="336 位 0/1 字符串（直接传入）")
    p_sched_g.add_argument(
        "--no-limit", action="store_true",
        help="不限时段（全 0）")

    p_tt_budget = sub.add_parser("toutiao_update_budget",
                                 help="批量修改巨量广告账户预算")
    p_tt_budget.add_argument("--media-account-ids", required=True,
                             help="媒体账户 ID（逗号分隔）")
    p_tt_budget.add_argument("--budgets", required=True,
                             help="预算金额（逗号分隔，与账户一一对应）")
    p_tt_budget.add_argument("--budget-mode", default="BUDGET_MODE_DAY",
                             choices=["BUDGET_MODE_DAY", "BUDGET_MODE_TOTAL"],
                             help="预算类型：日预算 / 总预算（默认日预算）")

    p_gdt_budget = sub.add_parser("gdt_update_budget",
                                  help="批量修改腾讯广告账户日预算")
    p_gdt_budget.add_argument("--media-account-ids", required=True,
                              help="媒体账户 ID（逗号分隔）")
    p_gdt_budget.add_argument("--daily-budgets", required=True,
                              help="日预算金额（逗号分隔，与账户一一对应）")

    p_gdt_date = sub.add_parser("gdt_update_dates",
                                help="批量修改腾讯广告投放日期")
    p_gdt_date.add_argument("--adgroup-ids", required=True,
                            help="广告组 ID（逗号分隔）")
    p_gdt_date.add_argument("--advertiser-ids", required=True,
                            help="广告主 ID（逗号分隔，与 adgroup-ids 一一对应）")
    p_gdt_date.add_argument("--begin-date", required=True,
                            help="开始日期 (YYYY-MM-DD)")
    p_gdt_date.add_argument("--end-date", required=True,
                            help="结束日期 (YYYY-MM-DD)")

    p_msgs = sub.add_parser("messages", help="站内信列表")
    p_msgs.add_argument("--page", type=int, default=1)
    p_msgs.add_argument("--page-size", type=int, default=10)

    args = parser.parse_args()

    if args.action == "setup":
        _cmd_setup(args)
        return
    if args.action == "clear-credentials":
        _cmd_clear_credentials(args)
        return

    email = args.email or get_injected_credential("cl", "email") or ""
    password = args.password or get_injected_credential("cl", "password") or ""
    if not email or not password:
        print("Error: 创量凭证未配置。请发送「设置创量凭证」进行配置。",
              file=sys.stderr)
        sys.exit(1)

    client = CLClient(email, password)
    if not client.login():
        sys.exit(1)

    result = None

    if args.action == "accounts":
        result = client.get_media_accounts(
            args.media_type, args.page, args.page_size)

    elif args.action == "menu":
        result = client.get_my_menu()

    elif args.action == "users":
        result = client.get_my_users()

    elif args.action == "media_types":
        result = {"code": 0, "data": MEDIA_TYPES}

    elif args.action == "materials":
        result = client.get_material_list(
            args.page, args.page_size,
            args.group_id, args.material_type)

    elif args.action == "material_search":
        result = client.search_material(
            args.keyword, args.page, args.page_size)

    elif args.action == "account_report":
        if args.start_date and args.end_date:
            sd, ed = args.start_date, args.end_date
        else:
            end = datetime.now()
            start = end - timedelta(days=args.days - 1)
            sd = start.strftime("%Y-%m-%d")
            ed = end.strftime("%Y-%m-%d")
        result = client.get_account_report(
            sd, ed, media_type=args.media_type,
            keyword=args.keyword, search_field=args.search_field,
            page=args.page, page_size=args.page_size)

    elif args.action == "material_report":
        if args.start_date and args.end_date:
            sd, ed = args.start_date, args.end_date
        else:
            end = datetime.now()
            start = end - timedelta(days=args.days - 1)
            sd = start.strftime("%Y-%m-%d")
            ed = end.strftime("%Y-%m-%d")
        result = client.get_material_report(
            sd, ed, media_type=args.media_type,
            keyword=args.keyword,
            page=args.page, page_size=args.page_size)

    elif args.action == "projects":
        if args.start_date and args.end_date:
            sd, ed = args.start_date, args.end_date
        else:
            end = datetime.now()
            start = end - timedelta(days=args.days - 1)
            sd = start.strftime("%Y-%m-%d")
            ed = end.strftime("%Y-%m-%d")
        result = client.get_project_list(
            sd, ed, args.page, args.page_size,
            keyword=args.keyword, status=args.status)

    elif args.action == "gdt_ads":
        if args.start_date and args.end_date:
            sd, ed = args.start_date, args.end_date
        else:
            end = datetime.now()
            start = end - timedelta(days=args.days - 1)
            sd = start.strftime("%Y-%m-%d")
            ed = end.strftime("%Y-%m-%d")
        result = client.get_gdt_ad_list(sd, ed, args.page, args.page_size)

    elif args.action == "update_status":
        ids = [s.strip() for s in args.ids.split(",") if s.strip()]
        result = client.update_project_status(ids, args.status)

    elif args.action == "toutiao_update_project_schedule":
        pids = [s.strip() for s in args.project_ids.split(",") if s.strip()]
        mids = [int(s.strip()) for s in args.media_account_ids.split(",")
                if s.strip()]
        if len(pids) != len(mids):
            print("Error: project-ids 和 media-account-ids 数量必须一致",
                  file=sys.stderr)
            sys.exit(1)
        if args.no_limit:
            sched = '0' * 336
        elif args.schedule_raw:
            sched = args.schedule_raw
        else:
            sched = build_schedule_time(args.schedule)
        if len(sched) != 336 or set(sched) - {'0', '1'}:
            print("Error: schedule_time 必须是 336 位 0/1 字符串",
                  file=sys.stderr)
            sys.exit(1)
        result = client.update_project_schedule(pids, mids, sched)
        if result.get("code") == 0:
            ok = client.wait_missions_done(["update_schedule_time"],
                                           timeout=60)
            result["_async_done"] = ok
            if not ok:
                print("Warning: async task did not complete within 60s",
                      file=sys.stderr)

    elif args.action == "toutiao_update_budget":
        mids = [s.strip() for s in args.media_account_ids.split(",")
                if s.strip()]
        budgets = [s.strip() for s in args.budgets.split(",") if s.strip()]
        if len(mids) != len(budgets):
            print("Error: media-account-ids 和 budgets 数量必须一致",
                  file=sys.stderr)
            sys.exit(1)
        result = client.update_toutiao_budget(
            mids, budgets, args.budget_mode)

    elif args.action == "gdt_update_budget":
        mids = [s.strip() for s in args.media_account_ids.split(",")
                if s.strip()]
        dbs = [int(s.strip()) for s in args.daily_budgets.split(",")
               if s.strip()]
        if len(mids) != len(dbs):
            print("Error: media-account-ids 和 daily-budgets 数量必须一致",
                  file=sys.stderr)
            sys.exit(1)
        result = client.update_gdt_budget(mids, dbs)

    elif args.action == "gdt_update_dates":
        ag_ids = [s.strip() for s in args.adgroup_ids.split(",") if s.strip()]
        adv_ids = [s.strip() for s in args.advertiser_ids.split(",") if s.strip()]
        if len(ag_ids) != len(adv_ids):
            print("Error: adgroup-ids 和 advertiser-ids 数量必须一致",
                  file=sys.stderr)
            sys.exit(1)
        result = client.update_gdt_ad_dates(
            ag_ids, adv_ids, args.begin_date, args.end_date)

    elif args.action == "messages":
        result = client.get_messages(args.page, args.page_size)

    if result is not None:
        json.dump(result, sys.stdout, ensure_ascii=False, indent=2)
        print()


if __name__ == "__main__":
    main()
