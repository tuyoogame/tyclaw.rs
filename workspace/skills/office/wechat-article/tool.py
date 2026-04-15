"""
微信公众号文章工具
搜索公众号、获取文章列表和全文内容

用法:
  python skills/wechat-article/tool.py status
  python skills/wechat-article/tool.py search --query "公众号名称"
  python skills/wechat-article/tool.py list --fakeid xxx --count 10
  python skills/wechat-article/tool.py read --url "https://mp.weixin.qq.com/s/..."
"""

import argparse
import html as html_module
import json
import os
import re
import sys
import time
from datetime import datetime

import requests

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "../.."))
from tools.utils import get_injected_credential

MP_BASE_URL = "https://mp.weixin.qq.com"

BROWSER_HEADERS = {
    "User-Agent": (
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) "
        "AppleWebKit/537.36 (KHTML, like Gecko) "
        "Chrome/120.0.0.0 Safari/537.36"
    ),
    "Accept": "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
    "Accept-Language": "zh-CN,zh;q=0.9,en;q=0.8",
}

_NO_CRED_MSG = "微信公众号未绑定，请点击下方链接完成扫码绑定后重试。"


# ---------------------------------------------------------------------------
# Credentials
# ---------------------------------------------------------------------------

def _request_bind_url() -> str | None:
    """通过 Bot Proxy 获取微信绑定 URL（OAuth-like 流程）。"""
    proxy_url = os.environ.get("_TYCLAW_DT_PROXY_URL", "")
    proxy_token = os.environ.get("_TYCLAW_DT_PROXY_TOKEN", "")
    if not proxy_url or not proxy_token:
        return None
    base = proxy_url.rsplit("/api/", 1)[0]
    try:
        resp = requests.post(
            f"{base}/api/wechat-bind-url",
            json={"token": proxy_token},
            timeout=10,
        )
        if resp.ok:
            return resp.json().get("bind_url")
    except Exception:
        pass
    return None


def _get_wechat_creds() -> dict | None:
    token = get_injected_credential("wechat", "token")
    cookie = get_injected_credential("wechat", "cookie")
    if not token or not cookie:
        return None
    expire_time = get_injected_credential("wechat", "expire_time")
    if expire_time:
        try:
            if int(expire_time) < int(time.time() * 1000):
                return None
        except (ValueError, TypeError):
            pass
    return {
        "token": token,
        "cookie": cookie,
        "fakeid": get_injected_credential("wechat", "fakeid") or "",
        "nickname": get_injected_credential("wechat", "nickname") or "",
        "expire_time": expire_time or "",
    }


def _require_creds() -> dict | None:
    """获取凭证，无凭证时打印绑定链接并返回 None。"""
    creds = _get_wechat_creds()
    if creds:
        return creds
    bind_url = _request_bind_url()
    if bind_url:
        print(f"{_NO_CRED_MSG}\n\n绑定链接: {bind_url}")
    else:
        print(f"{_NO_CRED_MSG}\n（无法生成绑定链接，请联系管理员）")
    return None


def _mp_headers(cookie: str) -> dict:
    return {
        "User-Agent": BROWSER_HEADERS["User-Agent"],
        "Referer": "https://mp.weixin.qq.com/",
        "Cookie": cookie,
    }


def _probe_session(creds: dict) -> bool:
    """向微信服务端发一个轻量请求，验证 session 是否真正有效。"""
    try:
        resp = requests.get(
            f"{MP_BASE_URL}/cgi-bin/searchbiz",
            params={
                "action": "search_biz",
                "token": creds["token"],
                "lang": "zh_CN",
                "f": "json",
                "ajax": 1,
                "random": time.time(),
                "query": creds.get("nickname") or "test",
                "begin": 0,
                "count": 1,
            },
            headers=_mp_headers(creds["cookie"]),
            timeout=10,
        )
        result = resp.json()
        ret = result.get("base_resp", {}).get("ret", -1)
        return ret == 0
    except Exception:
        return False


_SESSION_EXPIRED_MSG = "登录已过期（服务端 session 失效），请重新扫码登录"


# ---------------------------------------------------------------------------
# Sub-commands
# ---------------------------------------------------------------------------

def cmd_status(_args):
    creds = _require_creds()
    if not creds:
        return

    expire_ms = int(creds.get("expire_time", 0) or 0)
    if expire_ms > 0:
        expire_dt = datetime.fromtimestamp(expire_ms / 1000)
        remaining = expire_dt - datetime.now()
        hours_left = remaining.total_seconds() / 3600
        local_status = f"有效（剩余 {hours_left:.1f} 小时）" if hours_left > 0 else "已过期"
    else:
        local_status = "未知"

    server_ok = _probe_session(creds)

    print(f"登录状态: {'有效' if server_ok else '已失效'}")
    if not server_ok:
        print(f"  ⚠ {_SESSION_EXPIRED_MSG}")
        print(f"  （本地记录: {local_status}）")
    else:
        print(f"  本地过期时间: {local_status}")
    print(f"公众号: {creds.get('nickname', '未知')}")
    print(f"FakeID: {creds.get('fakeid', '未知')}")
    if expire_ms > 0:
        print(f"过期时间: {datetime.fromtimestamp(expire_ms / 1000).strftime('%Y-%m-%d %H:%M')}")


def cmd_search(args):
    creds = _require_creds()
    if not creds:
        return

    resp = requests.get(
        f"{MP_BASE_URL}/cgi-bin/searchbiz",
        params={
            "action": "search_biz",
            "token": creds["token"],
            "lang": "zh_CN",
            "f": "json",
            "ajax": 1,
            "random": time.time(),
            "query": args.query,
            "begin": 0,
            "count": 5,
        },
        headers=_mp_headers(creds["cookie"]),
        timeout=15,
    )
    result = resp.json()

    if result.get("base_resp", {}).get("ret") != 0:
        err = result.get("base_resp", {}).get("err_msg", "unknown error")
        print(f"Error: search failed — {err}", file=sys.stderr)
        if "login" in err.lower() or result["base_resp"]["ret"] == 200003:
            print("登录已过期，请重新扫码登录")
        return

    accounts = result.get("list", [])
    if not accounts:
        print(f"未找到匹配「{args.query}」的公众号")
        return

    print(f"搜索「{args.query}」找到 {len(accounts)} 个公众号:\n")
    for acc in accounts:
        stype = {0: "订阅号", 1: "订阅号", 2: "服务号"}.get(
            acc.get("service_type", 0), "未知")
        print(f"  名称: {acc.get('nickname', '')}")
        print(f"  FakeID: {acc.get('fakeid', '')}")
        alias = acc.get("alias", "")
        if alias:
            print(f"  微信号: {alias}")
        print(f"  类型: {stype}")
        print()


def cmd_list(args):
    creds = _require_creds()
    if not creds:
        return

    is_searching = bool(args.keyword)
    params = {
        "sub": "search" if is_searching else "list",
        "search_field": "7" if is_searching else "null",
        "begin": args.begin,
        "count": args.count,
        "query": args.keyword or "",
        "fakeid": args.fakeid,
        "type": "101_1",
        "free_publish_type": 1,
        "sub_action": "list_ex",
        "token": creds["token"],
        "lang": "zh_CN",
        "f": "json",
        "ajax": 1,
    }

    resp = requests.get(
        f"{MP_BASE_URL}/cgi-bin/appmsgpublish",
        params=params,
        headers=_mp_headers(creds["cookie"]),
        timeout=30,
    )
    result = resp.json()

    base_resp = result.get("base_resp", {})
    if base_resp.get("ret") != 0:
        err = base_resp.get("err_msg", "unknown error")
        print(f"Error: {err}", file=sys.stderr)
        if "login" in err.lower() or base_resp.get("ret") == 200003:
            print("登录已过期，请重新扫码登录")
        return

    publish_page = result.get("publish_page", {})
    if isinstance(publish_page, str):
        publish_page = json.loads(publish_page)

    publish_list = publish_page.get("publish_list", [])
    total = publish_page.get("total_count", 0)

    articles = []
    for item in publish_list:
        publish_info = item.get("publish_info", {})
        if isinstance(publish_info, str):
            publish_info = json.loads(publish_info)
        if not isinstance(publish_info, dict):
            continue
        for article in publish_info.get("appmsgex", []):
            articles.append(article)

    if not articles and total == 0 and not _probe_session(creds):
        print(f"Error: {_SESSION_EXPIRED_MSG}", file=sys.stderr)
        sys.exit(1)

    keyword_hint = f"（关键词: {args.keyword}）" if args.keyword else ""
    print(f"共 {total} 篇文章{keyword_hint}，当前第 {args.begin + 1}-{args.begin + len(articles)} 篇:\n")

    for i, a in enumerate(articles, start=args.begin + 1):
        ts = a.get("create_time", 0)
        date_str = datetime.fromtimestamp(ts).strftime("%Y-%m-%d") if ts else "未知"
        print(f"  [{i}] {a.get('title', '无标题')}")
        print(f"      日期: {date_str}")
        print(f"      链接: {a.get('link', '')}")
        digest = a.get("digest", "")
        if digest:
            print(f"      摘要: {digest[:60]}")
        print()


def _html_to_text(html_str: str) -> str:
    """HTML → 可读纯文本（无外部依赖）"""
    text = re.sub(r"<br\s*/?>", "\n", html_str, flags=re.IGNORECASE)
    text = re.sub(r"</p>", "\n", text, flags=re.IGNORECASE)
    text = re.sub(r"<hr[^>]*>", "\n---\n", text, flags=re.IGNORECASE)
    text = re.sub(r"<[^>]+>", "", text)
    text = html_module.unescape(text)
    text = re.sub(r"[ \t]+", " ", text)
    text = re.sub(r"\n{3,}", "\n\n", text)
    return text.strip()


def _fetch_article_html(url: str) -> str:
    """获取文章 HTML，优先使用 curl_cffi 模拟 Chrome TLS 指纹"""
    headers = {
        **BROWSER_HEADERS,
        "Referer": "https://mp.weixin.qq.com/",
        "Sec-Ch-Ua": '"Not_A Brand";v="8", "Chromium";v="120"',
        "Sec-Ch-Ua-Mobile": "?0",
        "Sec-Fetch-Dest": "document",
        "Sec-Fetch-Mode": "navigate",
    }
    try:
        from curl_cffi.requests import Session as CurlSession
        with CurlSession(impersonate="chrome120") as s:
            resp = s.get(url, headers=headers, timeout=60,
                         allow_redirects=True, verify=False)
            resp.raise_for_status()
            return resp.text
    except ImportError:
        pass

    resp = requests.get(url, headers=headers, timeout=60, allow_redirects=True)
    resp.raise_for_status()
    return resp.text


def cmd_read(args):
    print(f"Fetching article: {args.url[:80]}...", file=sys.stderr)
    try:
        html = _fetch_article_html(args.url)
    except Exception as e:
        print(f"Error: failed to fetch article — {e}", file=sys.stderr)
        return

    if "环境异常" in html or "去验证" in html:
        print("触发微信安全验证，请稍后重试或降低请求频率。")
        return
    if "该内容已被发布者删除" in html:
        print("该文章已被发布者删除。")
        return

    title = ""
    m = re.search(r'var\s+msg_title\s*=\s*["\']([^"\']*)["\']', html)
    if m:
        title = html_module.unescape(m.group(1))
    if not title:
        m = re.search(r"<title>(.*?)</title>", html, re.IGNORECASE | re.DOTALL)
        if m:
            title = html_module.unescape(m.group(1).strip())

    author = ""
    m = re.search(r'<meta\s+name="author"\s+content="([^"]*)"', html)
    if m:
        author = html_module.unescape(m.group(1))
    if not author:
        m = re.search(r'id="js_name"[^>]*>(.*?)</a>', html, re.DOTALL)
        if m:
            author = m.group(1).strip()

    publish_time = 0
    m = re.search(r'var\s+ct\s*=\s*"(\d+)"', html)
    if m:
        publish_time = int(m.group(1))
    if not publish_time:
        m = re.search(r"var\s+publish_time\s*=\s*\"(\d+)\"", html)
        if m:
            publish_time = int(m.group(1))

    content_html = ""
    m = re.search(
        r'id="js_content"[^>]*>([\s\S]*?)</div>\s*(?:<div[^>]*class="rich_media_tool|<script)',
        html, re.IGNORECASE,
    )
    if m:
        content_html = m.group(1).strip()

    images = re.findall(r'data-src="(https?://mmbiz[^"]+)"', html)
    images += [
        u for u in re.findall(r'src="(https?://mmbiz[^"]+)"', html)
        if u not in images
    ]

    if args.format == "html":
        print(f"# {title}")
        if author:
            print(f"作者: {author}")
        if publish_time:
            print(f"发布时间: {datetime.fromtimestamp(publish_time)}")
        print()
        print(content_html)
    else:
        plain = _html_to_text(content_html) if content_html else "(无法提取正文内容)"
        print(f"# {title}")
        if author:
            print(f"作者: {author}")
        if publish_time:
            print(f"发布时间: {datetime.fromtimestamp(publish_time)}")
        print(f"图片: {len(images)} 张")
        print()
        print(plain)


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main():
    parser = argparse.ArgumentParser(description="微信公众号文章工具")
    sub = parser.add_subparsers(dest="command")

    sub.add_parser("status", help="检查登录状态")

    p_search = sub.add_parser("search", help="搜索公众号")
    p_search.add_argument("--query", required=True, help="搜索关键词")

    p_list = sub.add_parser("list", help="获取文章列表")
    p_list.add_argument("--fakeid", required=True, help="公众号 FakeID")
    p_list.add_argument("--begin", type=int, default=0, help="偏移量")
    p_list.add_argument("--count", type=int, default=10, help="获取数量")
    p_list.add_argument("--keyword", default="", help="搜索关键词")

    p_read = sub.add_parser("read", help="获取文章全文")
    p_read.add_argument("--url", required=True, help="文章链接")
    p_read.add_argument("--format", choices=["text", "html"], default="text",
                         help="输出格式")

    args = parser.parse_args()

    commands = {
        "status": cmd_status,
        "search": cmd_search,
        "list": cmd_list,
        "read": cmd_read,
    }

    if not args.command:
        parser.print_help()
        return

    commands[args.command](args)


if __name__ == "__main__":
    main()
