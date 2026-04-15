#!/usr/bin/env python3
"""七麦数据 (Qimai) API Client — APP榜单、搜索、关键词排名查询"""

import argparse
import base64
import json
import os
import sys
import time
from datetime import datetime, timedelta
from pathlib import Path
from urllib.parse import quote

import requests

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
from utils import (get_injected_credential, save_user_credentials, clear_user_credentials,
                   sync_credential_env, clear_credential_env)

API_BASE = "https://api.qimai.cn"
WEB_BASE = "https://www.qimai.cn"
XOR_KEY = "00000008d78d46a"
TIMESTAMP_OFFSET = 1105735 + 1515125653845


def _xor_encrypt(text: str, key: str, shift: int = 10) -> str:
    """XOR encryption with shifted key index (七麦 k 函数的 Python 实现)"""
    result = []
    key_len = len(key)
    for i, ch in enumerate(text):
        result.append(chr(ord(ch) ^ ord(key[(i + shift) % key_len])))
    return "".join(result)


def _build_analysis(url_path: str, params: dict) -> str:
    """生成 analysis 加密参数

    算法：
    1. params values sorted + joined → base64
    2. 拼接 @# + url_path + @# + timestamp + @# + 1
    3. XOR(key=00000008d78d46a, shift=10) → base64
    """
    param_values = sorted(str(v) for v in params.values())
    joined = "".join(param_values)
    b64_params = base64.b64encode(joined.encode("utf-8")).decode("utf-8")

    ts = int(time.time() * 1000) - TIMESTAMP_OFFSET
    raw = f"{b64_params}@#{url_path}@#{ts}@#1"

    xored = _xor_encrypt(raw, XOR_KEY)
    return base64.b64encode(xored.encode("latin-1")).decode("utf-8")


def _session_cache_path() -> Path:
    personal = os.environ.get("TYCLAW_PERSONAL_DIR", "")
    if personal:
        return Path(personal) / ".cache" / "qimai_session.json"
    return Path.home() / ".cache" / "qimai_session.json"


# 榜单类型
BRAND_TYPES = {
    "free": 1,       # 免费榜
    "paid": 0,       # 付费榜
    "grossing": 2,   # 畅销榜
}

# 设备类型
DEVICE_TYPES = ["iphone", "ipad", "mac", "android"]

# 常用游戏分类 (iOS)
GENRE_MAP = {
    "all": "36",           # 全部(应用+游戏)
    "app": "6000",         # 全部应用
    "game": "6014",        # 全部游戏
    "action": "7001",      # 动作游戏
    "adventure": "7002",   # 冒险游戏
    "casual": "7003",      # 休闲游戏
    "board": "7004",       # 棋盘游戏
    "card": "7005",        # 卡牌游戏
    "casino": "7006",      # 博彩游戏
    "puzzle": "7012",      # 益智解谜
    "racing": "7013",      # 赛车游戏
    "role": "7014",        # 角色扮演
    "simulation": "7015",  # 模拟游戏
    "sports": "7016",      # 体育游戏
    "strategy": "7017",    # 策略游戏
    "word": "7019",        # 文字游戏
}


class QimaiClient:
    """七麦数据 API 客户端"""

    def __init__(self, email: str, password: str):
        self.email = email
        self.password = password
        self.session = requests.Session()
        self.session.headers.update({
            "User-Agent": "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) "
                          "AppleWebKit/537.36 (KHTML, like Gecko) "
                          "Chrome/120.0.0.0 Safari/537.36",
            "Referer": f"{WEB_BASE}/rank",
            "Accept": "application/json, text/plain, */*",
            "Origin": WEB_BASE,
        })
        self.logged_in = False
        self._load_session()

    # ── session 持久化 ──

    def _save_session(self):
        cache = _session_cache_path()
        cache.parent.mkdir(parents=True, exist_ok=True)
        data = {
            "email": self.email,
            "cookies": self.session.cookies.get_dict(),
            "ts": time.time(),
        }
        cache.write_text(json.dumps(data))

    def _load_session(self):
        cache = _session_cache_path()
        if not cache.exists():
            return
        try:
            data = json.loads(cache.read_text())
            if data.get("email") != self.email:
                return
            if time.time() - data.get("ts", 0) > 7200:
                return
            for k, v in data["cookies"].items():
                self.session.cookies.set(k, v)
            self.logged_in = True
        except Exception:
            pass

    # ── 登录 ──

    def login(self) -> bool:
        """尝试登录七麦（密码方式，不需要验证码时可用）"""
        if self.logged_in and self._test_session():
            print("[login] session still valid, skipping login", file=sys.stderr)
            return True

        # 先访问首页拿 cookie
        try:
            self.session.get(f"{WEB_BASE}/rank", timeout=10)
        except Exception:
            pass

        url_path = "/account/signinForm"
        analysis = _build_analysis(url_path, {})

        resp = self.session.post(
            f"{API_BASE}{url_path}",
            data={
                "username": self.email,
                "password": self.password,
                "analysis": analysis,
            },
            timeout=15,
        )
        result = resp.json()

        if result.get("code") == 10000:
            self.logged_in = True
            self._save_session()
            username = result.get("userinfo", {}).get("username", "")
            print(f"[login] success as {username}", file=sys.stderr)
            return True

        msg = result.get("msg", result.get("message", "unknown error"))
        print(f"[login] failed: {msg}", file=sys.stderr)

        if "验证码" in str(msg):
            print("[login] captcha required - trying cookie-based auth",
                  file=sys.stderr)
        return False

    def _test_session(self) -> bool:
        """测试 session 是否有效"""
        try:
            url_path = "/account/userinfo"
            analysis = _build_analysis(url_path, {})
            resp = self.session.get(
                f"{API_BASE}{url_path}",
                params={"analysis": analysis},
                timeout=10,
            )
            data = resp.json()
            return data.get("code") == 10000
        except Exception:
            return False

    def _get(self, url_path: str, params: dict, timeout: int = 15) -> dict:
        """带 analysis 的 GET 请求"""
        analysis = _build_analysis(url_path, params)
        params["analysis"] = analysis
        resp = self.session.get(
            f"{API_BASE}{url_path}",
            params=params,
            timeout=timeout,
        )
        return resp.json()

    # ── 榜单查询 ──

    def rank(self, brand: str = "free", device: str = "iphone",
             country: str = "cn", genre: str = "game",
             date: str = "", page: int = 1) -> dict:
        """查询APP榜单

        Args:
            brand: free(免费榜) / paid(付费榜) / grossing(畅销榜)
            device: iphone / ipad / android
            country: 国家代码 (cn/us/jp/kr 等)
            genre: 分类 (game/app/action/role/strategy 等，也可传数字ID)
            date: 日期 (YYYY-MM-DD，默认今天)
            page: 页码 (1-10，每页20条)
        """
        brand_id = BRAND_TYPES.get(brand, brand)
        genre_id = GENRE_MAP.get(genre, genre)
        if not date:
            date = datetime.now().strftime("%Y-%m-%d")

        url_path = f"/rank/indexPlus/brand_id/{brand_id}"
        params = {
            "brand": "all",
            "device": device,
            "country": country,
            "genre": genre_id,
            "date": date,
            "page": str(page),
        }
        return self._get(url_path, params)

    # ── 搜索 ──

    def search(self, keyword: str, country: str = "cn",
               device: str = "iphone", page: int = 1) -> dict:
        """搜索APP"""
        url_path = "/search/index"
        params = {
            "search": keyword,
            "country": country,
            "device": device,
            "page": str(page),
        }
        return self._get(url_path, params)

    # ── APP详情 ──

    def app_info(self, app_id: str, country: str = "cn") -> dict:
        """查询APP基本信息"""
        url_path = "/app/appinfo"
        params = {
            "appid": app_id,
            "country": country,
        }
        return self._get(url_path, params)

    # ── APP 排名变化 ──

    def app_rank(self, app_id: str, country: str = "cn",
                 device: str = "iphone", brand: str = "free") -> dict:
        """查询APP排名变化趋势"""
        brand_id = BRAND_TYPES.get(brand, brand)
        url_path = "/rank/appRank"
        params = {
            "appid": app_id,
            "country": country,
            "device": device,
            "brand_id": str(brand_id),
        }
        return self._get(url_path, params)

    # ── 关键词排名 ──

    def keyword_rank(self, app_id: str, country: str = "cn",
                     device: str = "iphone") -> dict:
        """查询APP关键词排名（需登录）"""
        url_path = "/rank/keywordRank"
        params = {
            "appid": app_id,
            "country": country,
            "device": device,
        }
        return self._get(url_path, params)

    # ── 版本记录 ──

    def version_history(self, app_id: str, country: str = "cn") -> dict:
        """查询APP版本更新记录"""
        url_path = "/app/versionCompare"
        params = {
            "appid": app_id,
            "country": country,
        }
        return self._get(url_path, params)


# ── 输出格式化 ──

def _format_rank_list(data: dict) -> str:
    """格式化榜单数据"""
    app_list = data.get("list", data.get("rankInfo", []))
    if not app_list:
        return json.dumps(data, ensure_ascii=False, indent=2)

    lines = []
    lines.append("| 排名 | 总榜 | 应用名称 | AppID | 开发商 | 变化 |")
    lines.append("| --- | --- | --- | --- | --- | --- |")
    for app in app_list:
        idx = app.get("index", "")
        info = app.get("appInfo", {})
        class_rank = app.get("class", {}).get("ranking", "-")
        name = info.get("appName", "")
        app_id = info.get("appId", "")
        company = info.get("publisher", "")
        change = app.get("change", 0)
        change_str = f"+{change}" if change > 0 else str(change) if change != 0 else "-"
        lines.append(f"| {idx} | {class_rank} | {name} | {app_id} | {company} | {change_str} |")
    return "\n".join(lines)


def _format_search_results(data: dict) -> str:
    """格式化搜索结果"""
    app_list = data.get("appList", data.get("appInfo", []))
    if not app_list:
        return json.dumps(data, ensure_ascii=False, indent=2)

    keyword = data.get("wordInfo", {}).get("word", "")
    popularity = data.get("popularity", "")
    total = data.get("totalNum", len(app_list))
    header = f"搜索: {keyword}  热度: {popularity}  结果数: {total}\n\n"

    lines = [header]
    lines.append("| 应用名称 | AppID | 开发商 | 分类 |")
    lines.append("| --- | --- | --- | --- |")
    for item in app_list:
        info = item.get("appInfo", item)
        name = info.get("appName", "")
        app_id = info.get("appId", "")
        company = info.get("publisher", "")
        genre = item.get("genre", "")
        lines.append(f"| {name} | {app_id} | {company} | {genre} |")
    return "\n".join(lines)


# ── CLI ──

def _get_credentials(args) -> tuple[str, str]:
    email = getattr(args, "email", None) or get_injected_credential("qimai", "email")
    password = getattr(args, "password", None) or get_injected_credential("qimai", "password")
    if not email or not password:
        print("ERROR: Qimai credentials not configured. "
              "Run: python3 tools/qimai_query.py setup --email <email> --password <password>",
              file=sys.stderr)
        sys.exit(1)
    return email, password


def cmd_setup(args):
    staff_id = os.environ.get("TYCLAW_SENDER_STAFF_ID", "local")
    data = {"email": args.email, "password": args.password}
    save_user_credentials(staff_id, "qimai", data)
    sync_credential_env("qimai", data)
    print(json.dumps({"status": "ok", "message": f"Qimai credentials saved for {args.email}"},
                      ensure_ascii=False))


def cmd_clear_credentials(args):
    staff_id = os.environ.get("TYCLAW_SENDER_STAFF_ID", "local")
    changed = clear_user_credentials(staff_id, "qimai")
    clear_credential_env("qimai")
    _session_cache_path().unlink(missing_ok=True)
    print(json.dumps({"status": "ok", "cleared": changed}, ensure_ascii=False))


def cmd_rank(args):
    email, password = _get_credentials(args)
    client = QimaiClient(email, password)
    result = client.rank(
        brand=args.brand,
        device=args.device,
        country=args.country,
        genre=args.genre,
        date=args.date,
        page=args.page,
    )
    if result.get("code") == 10000:
        print(_format_rank_list(result))
    else:
        print(json.dumps(result, ensure_ascii=False, indent=2))


def cmd_search(args):
    email, password = _get_credentials(args)
    client = QimaiClient(email, password)
    result = client.search(
        keyword=args.keyword,
        country=args.country,
        device=args.device,
        page=args.page,
    )
    if result.get("code") == 10000:
        print(_format_search_results(result))
    else:
        print(json.dumps(result, ensure_ascii=False, indent=2))


def cmd_app_info(args):
    email, password = _get_credentials(args)
    client = QimaiClient(email, password)
    result = client.app_info(app_id=args.app_id, country=args.country)
    print(json.dumps(result, ensure_ascii=False, indent=2))


def cmd_app_rank(args):
    email, password = _get_credentials(args)
    client = QimaiClient(email, password)
    result = client.app_rank(
        app_id=args.app_id,
        country=args.country,
        device=args.device,
        brand=args.brand,
    )
    print(json.dumps(result, ensure_ascii=False, indent=2))


def cmd_keyword_rank(args):
    email, password = _get_credentials(args)
    client = QimaiClient(email, password)
    if not client.logged_in:
        client.login()
    result = client.keyword_rank(
        app_id=args.app_id,
        country=args.country,
        device=args.device,
    )
    print(json.dumps(result, ensure_ascii=False, indent=2))


def cmd_version_history(args):
    email, password = _get_credentials(args)
    client = QimaiClient(email, password)
    result = client.version_history(app_id=args.app_id, country=args.country)
    print(json.dumps(result, ensure_ascii=False, indent=2))


def main():
    parser = argparse.ArgumentParser(description="七麦数据查询工具")
    parser.add_argument("--email", help="七麦登录邮箱")
    parser.add_argument("--password", help="七麦登录密码")
    sub = parser.add_subparsers(dest="command")

    # setup
    p_setup = sub.add_parser("setup", help="配置七麦凭证")
    p_setup.add_argument("--email", required=True, dest="email")
    p_setup.add_argument("--password", required=True, dest="password")
    p_setup.set_defaults(func=cmd_setup)

    # clear-credentials
    p_clear = sub.add_parser("clear-credentials", help="清除凭证")
    p_clear.set_defaults(func=cmd_clear_credentials)

    # rank
    p_rank = sub.add_parser("rank", help="查询APP榜单")
    p_rank.add_argument("--brand", default="free",
                        choices=["free", "paid", "grossing"],
                        help="榜单类型")
    p_rank.add_argument("--device", default="iphone",
                        choices=DEVICE_TYPES, help="设备类型")
    p_rank.add_argument("--country", default="cn", help="国家代码")
    p_rank.add_argument("--genre", default="game", help="分类")
    p_rank.add_argument("--date", default="", help="日期 (YYYY-MM-DD)")
    p_rank.add_argument("--page", type=int, default=1, help="页码 (1-10)")
    p_rank.set_defaults(func=cmd_rank)

    # search
    p_search = sub.add_parser("search", help="搜索APP")
    p_search.add_argument("--keyword", required=True, help="搜索关键词")
    p_search.add_argument("--country", default="cn")
    p_search.add_argument("--device", default="iphone",
                          choices=DEVICE_TYPES)
    p_search.add_argument("--page", type=int, default=1)
    p_search.set_defaults(func=cmd_search)

    # app-info
    p_info = sub.add_parser("app-info", help="查询APP信息")
    p_info.add_argument("--app-id", required=True, help="App Store ID")
    p_info.add_argument("--country", default="cn")
    p_info.set_defaults(func=cmd_app_info)

    # app-rank
    p_arank = sub.add_parser("app-rank", help="查询APP排名趋势")
    p_arank.add_argument("--app-id", required=True)
    p_arank.add_argument("--country", default="cn")
    p_arank.add_argument("--device", default="iphone",
                         choices=DEVICE_TYPES)
    p_arank.add_argument("--brand", default="free",
                         choices=["free", "paid", "grossing"])
    p_arank.set_defaults(func=cmd_app_rank)

    # keyword-rank
    p_kw = sub.add_parser("keyword-rank", help="查询关键词排名（需登录）")
    p_kw.add_argument("--app-id", required=True)
    p_kw.add_argument("--country", default="cn")
    p_kw.add_argument("--device", default="iphone",
                      choices=DEVICE_TYPES)
    p_kw.set_defaults(func=cmd_keyword_rank)

    # version-history
    p_ver = sub.add_parser("version-history", help="查询版本记录")
    p_ver.add_argument("--app-id", required=True)
    p_ver.add_argument("--country", default="cn")
    p_ver.set_defaults(func=cmd_version_history)

    args = parser.parse_args()
    if not args.command:
        parser.print_help()
        sys.exit(1)

    args.func(args)


if __name__ == "__main__":
    main()
