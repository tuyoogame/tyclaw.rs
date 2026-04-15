#!/usr/bin/env python3
"""AdXray DataEye API Client — login, sign, and query advertising data."""

import argparse
import hashlib
import json
import os
import re
import sys
import time
from datetime import datetime, timedelta
from pathlib import Path

import requests

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
from utils import (get_injected_credential, save_user_credentials, clear_user_credentials,
                   sync_credential_env, clear_credential_env)

SIGN_KEY = "g:%w0k7&q1v9^tRnLz!M"
BASE_URL = "https://adxray.dataeye.com"
def _session_cache_path(name: str) -> Path:
    personal = os.environ.get("TYCLAW_PERSONAL_DIR", "")
    if personal:
        return Path(personal) / ".cache" / name
    return Path.home() / ".cache" / name

SESSION_CACHE = _session_cache_path("adxray_session.json")


def md5(text: str) -> str:
    return hashlib.md5(text.encode()).hexdigest()


def compute_sign(params: dict) -> str:
    keys = sorted(k for k in params if k not in ("sign", "token") and params[k] is not None)
    parts = []
    for k in keys:
        v = params[k]
        if isinstance(v, str):
            v = v.strip()
        if v is None:
            v = ""
        parts.append(f"{k}={v}")
    raw = "&".join(parts) + f"&key={SIGN_KEY}"
    return md5(raw).upper()


def make_s_header() -> str:
    now = datetime.now()
    date_str = f"{now.year}/{now.month}/{now.day}"
    return md5(date_str)


class AdXrayClient:
    def __init__(self, email: str, password: str):
        self.email = email
        self.password_md5 = md5(password)
        self.session = requests.Session()
        self.session.headers.update({
            "User-Agent": "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36",
        })
        self.user_key = None
        self._load_session()

    # ── session persistence ──

    def _save_session(self):
        SESSION_CACHE.parent.mkdir(parents=True, exist_ok=True)
        data = {
            "email": self.email,
            "cookies": self.session.cookies.get_dict(),
            "user_key": self.user_key,
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
            if time.time() - data.get("ts", 0) > 3600:
                return
            for k, v in data["cookies"].items():
                self.session.cookies.set(k, v)
            self.user_key = data.get("user_key")
        except Exception:
            pass

    # ── login ──

    def _solve_captcha(self, img_bytes: bytes) -> str:
        try:
            import ddddocr
            ocr = ddddocr.DdddOcr(show_ad=False)
            return ocr.classification(img_bytes).strip()
        except ImportError:
            tmp = Path("/tmp/adxray_captcha.png")
            tmp.write_bytes(img_bytes)
            print(f"[captcha] saved to {tmp}, please read and enter:", file=sys.stderr)
            return input("captcha> ").strip()

    def login(self, max_retries: int = 5) -> bool:
        if self.user_key and self._test_session():
            print("[login] session still valid, skipping login", file=sys.stderr)
            return True

        for attempt in range(1, max_retries + 1):
            img = self.session.get(f"{BASE_URL}/user/getVerifyCode").content
            code = self._solve_captcha(img)
            if not code:
                continue

            resp = self.session.post(
                f"{BASE_URL}/user/login",
                data={
                    "accountId": self.email,
                    "password": self.password_md5,
                    "vCode": code,
                },
            )
            body = resp.json()
            if body.get("statusCode") == 200:
                self._fetch_user_key()
                self._save_session()
                print(f"[login] success on attempt {attempt}", file=sys.stderr)
                return True
            print(f"[login] attempt {attempt} failed: {body.get('msg')}", file=sys.stderr)

        print("[login] all attempts failed", file=sys.stderr)
        return False

    def _fetch_user_key(self):
        html = self.session.get(f"{BASE_URL}/index/home").text
        m = re.search(r'userKey:\s*"([^"]+)"', html)
        if m:
            self.user_key = m.group(1)

    def _test_session(self) -> bool:
        try:
            html = self.session.get(f"{BASE_URL}/index/home", timeout=10).text
            m = re.search(r"isLogin:\s*'(\d)'", html)
            if m and m.group(1) == "1":
                if not self.user_key:
                    self._fetch_user_key()
                return True
        except Exception:
            pass
        return False

    # ── API calls ──

    def _post(self, path: str, params: dict) -> dict:
        params["thisTimes"] = int(time.time() * 1000 / 100)
        params["sign"] = compute_sign(params)
        params["token"] = self.user_key
        resp = self.session.post(
            f"{BASE_URL}{path}",
            data=params,
            headers={
                "Content-Type": "application/x-www-form-urlencoded;charset=UTF-8",
                "s": make_s_header(),
            },
        )
        body = resp.json()
        if body.get("statusCode") == 401:
            print("[api] session expired, re-logging in...", file=sys.stderr)
            SESSION_CACHE.unlink(missing_ok=True)
            if self.login():
                params["token"] = self.user_key
                params["sign"] = compute_sign(params)
                resp = self.session.post(
                    f"{BASE_URL}{path}",
                    data=params,
                    headers={
                        "Content-Type": "application/x-www-form-urlencoded;charset=UTF-8",
                        "s": make_s_header(),
                    },
                )
                body = resp.json()
        return body

    def get_product_info(self, product_id: int) -> dict:
        return self._post("/product/getProductInfo", {"productId": product_id})

    def get_trend(self, product_id: int, start_date: str, end_date: str) -> dict:
        return self._post("/product/listTrendByProduct", {
            "productId": product_id,
            "startDate": start_date,
            "endDate": end_date,
        })

    def get_media_distribution(self, product_id: int, start_date: str, end_date: str) -> dict:
        return self._post("/product/listMediaDistributionV2", {
            "productId": product_id,
            "startDate": start_date,
            "endDate": end_date,
        })

    def get_position_distribution(self, product_id: int, start_date: str, end_date: str) -> dict:
        return self._post("/product/listPositionDistributionV2", {
            "productId": product_id,
            "startDate": start_date,
            "endDate": end_date,
        })

    def get_audience_analysis(self, product_id: int, product_name: str) -> dict:
        return self._post("/product/audienceAnalysis", {
            "productId": product_id,
            "productName": product_name,
        })

    def get_hot_ranking(self, start_date: str, end_date: str,
                        page: int = 1, size: int = 50) -> dict:
        return self._post("/product/listHotProductRanking", {
            "searchType": 1,
            "pageId": page,
            "pageSize": size,
            "top": 500,
            "startDate": start_date,
            "endDate": end_date,
            "chargesType": "",
            "adForm": "INFO_FLOW",
        })

    def get_new_ranking(self, start_date: str, end_date: str,
                        page: int = 1, size: int = 50) -> dict:
        return self._post("/product/listNewProductRanking", {
            "searchType": 1,
            "pageId": page,
            "pageSize": size,
            "top": 500,
            "startDate": start_date,
            "endDate": end_date,
            "chargesType": "",
            "adForm": "INFO_FLOW",
        })

    def search_product(self, keyword: str, top: int = 10) -> dict:
        return self._post("/search/quickSearch", {
            "searchKey": keyword,
            "top": top,
            "isHighLight": "true",
        })


def date_range(days: int) -> tuple[str, str]:
    end = datetime.now()
    start = end - timedelta(days=days - 1)
    return start.strftime("%Y-%m-%d"), end.strftime("%Y-%m-%d")


def _cmd_setup(args):
    staff_id = os.environ.get("TYCLAW_SENDER_STAFF_ID", "")
    if not staff_id:
        print("Error: TYCLAW_SENDER_STAFF_ID env var required", file=sys.stderr)
        sys.exit(1)
    data = {"email": args.setup_email, "password": args.setup_password}
    save_user_credentials(staff_id, "adx", data)
    sync_credential_env("adx", data)
    print(f"ADX credentials saved for {staff_id}")


def _cmd_clear_credentials(_args):
    staff_id = os.environ.get("TYCLAW_SENDER_STAFF_ID", "")
    if not staff_id:
        print("Error: TYCLAW_SENDER_STAFF_ID env var required", file=sys.stderr)
        sys.exit(1)
    if clear_user_credentials(staff_id, "adx"):
        clear_credential_env("adx")
        print(f"ADX credentials cleared for {staff_id}")
    else:
        print(f"No ADX credentials found for {staff_id}")


def main():
    parser = argparse.ArgumentParser(description="AdXray DataEye CLI")
    parser.add_argument("--email", default="")
    parser.add_argument("--password", default="")
    sub = parser.add_subparsers(dest="action", required=True)

    p_setup = sub.add_parser("setup", help="Set ADX credentials")
    p_setup.add_argument("--email", required=True, dest="setup_email")
    p_setup.add_argument("--password", required=True, dest="setup_password")
    sub.add_parser("clear-credentials", help="Clear ADX credentials")

    p_info = sub.add_parser("product_info")
    p_info.add_argument("--product-id", type=int, required=True)

    p_trend = sub.add_parser("trend")
    p_trend.add_argument("--product-id", type=int, required=True)
    p_trend.add_argument("--days", type=int, default=7)
    p_trend.add_argument("--start-date")
    p_trend.add_argument("--end-date")

    p_media = sub.add_parser("media_dist")
    p_media.add_argument("--product-id", type=int, required=True)
    p_media.add_argument("--days", type=int, default=7)
    p_media.add_argument("--start-date")
    p_media.add_argument("--end-date")

    p_hot = sub.add_parser("hot_ranking")
    p_hot.add_argument("--days", type=int, default=7)
    p_hot.add_argument("--page", type=int, default=1)
    p_hot.add_argument("--size", type=int, default=50)

    p_new = sub.add_parser("new_ranking")
    p_new.add_argument("--days", type=int, default=7)
    p_new.add_argument("--page", type=int, default=1)
    p_new.add_argument("--size", type=int, default=50)

    p_search = sub.add_parser("search")
    p_search.add_argument("--keyword", required=True)

    args = parser.parse_args()

    if args.action == "setup":
        _cmd_setup(args)
        return
    if args.action == "clear-credentials":
        _cmd_clear_credentials(args)
        return

    email = args.email or get_injected_credential("adx", "email") or ""
    password = args.password or get_injected_credential("adx", "password") or ""
    if not email or not password:
        print("Error: ADX credentials not found. "
              "请发送「设置ADX凭证」进行配置。",
              file=sys.stderr)
        sys.exit(1)

    client = AdXrayClient(email, password)
    if not client.login():
        sys.exit(1)

    if args.action == "product_info":
        result = client.get_product_info(args.product_id)
    elif args.action == "trend":
        sd = args.start_date
        ed = args.end_date
        if not sd or not ed:
            sd, ed = date_range(args.days)
        result = client.get_trend(args.product_id, sd, ed)
    elif args.action == "media_dist":
        sd = args.start_date
        ed = args.end_date
        if not sd or not ed:
            sd, ed = date_range(args.days)
        result = client.get_media_distribution(args.product_id, sd, ed)
    elif args.action == "hot_ranking":
        sd, ed = date_range(args.days)
        result = client.get_hot_ranking(sd, ed, args.page, args.size)
    elif args.action == "new_ranking":
        sd, ed = date_range(args.days)
        result = client.get_new_ranking(sd, ed, args.page, args.size)
    elif args.action == "search":
        result = client.search_product(args.keyword)
    else:
        parser.print_help()
        sys.exit(1)

    json.dump(result, sys.stdout, ensure_ascii=False, indent=2)
    print()


if __name__ == "__main__":
    main()
