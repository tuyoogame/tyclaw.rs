"""
钉钉 OAuth 授权与 unionId 映射管理

供 dingtalk_sheet.py / dingtalk_doc.py / dingtalk_wiki.py import 使用，也可独立运行 CLI 子命令。

两种 token 完全隔离：
- 应用 token (App Token): get_dingtalk_token() in utils.py，用于所有 API 调用
- 个人 token (User Token): 仅在 OAuth 流程中消费一次拿 unionId，不持久化

CLI 用法:
  python tools/dingtalk_auth.py check --user-id <id>
  python tools/dingtalk_auth.py auth-url --user-id <id>
  python tools/dingtalk_auth.py resolve --user-id <id> --access-token <token>
"""

import argparse
import json
import os
import sys
import urllib.parse

import requests

from utils import load_config, format_json, get_project_root, load_user_credentials, save_user_credentials

BASE_URL = "https://api.dingtalk.com"


# ---------------------------------------------------------------------------
# unionId 读写（per-user credentials.yaml）
# ---------------------------------------------------------------------------

def get_unionid(staff_id: str) -> str | None:
    """从 per-user credentials.yaml 读取 unionId"""
    creds = load_user_credentials(staff_id)
    return creds.get("dingtalk", {}).get("union_id") or None


# ---------------------------------------------------------------------------
# OAuth 授权 URL 构造
# ---------------------------------------------------------------------------

def build_auth_url(config: dict | None, user_id: str) -> str:
    """构造 OAuth 授权 URL。优先从 env 读参数（Docker 容器内），回退 config。"""
    client_id = os.environ.get("_TYCLAW_DT_CLIENT_ID", "")
    redirect_uri = os.environ.get("_TYCLAW_DT_OAUTH_REDIRECT_URI", "")

    if not client_id or not redirect_uri:
        if config is None:
            config = load_config()
        dt = config.get("dingtalk", {})
        client_id = client_id or dt.get("client_id", "")
        redirect_uri = redirect_uri or dt.get("oauth_redirect_uri", "")

    if not client_id:
        raise ValueError("dingtalk.client_id not configured")
    if not redirect_uri:
        oauth_port = config.get("dingtalk", {}).get("oauth_port", 9080) if config else 9080
        redirect_uri = f"http://localhost:{oauth_port}/oauth/dingtalk/callback"

    params = {
        "redirect_uri": redirect_uri,
        "response_type": "code",
        "client_id": client_id,
        "scope": "openid",
        "prompt": "consent",
        "state": user_id,
    }

    if not config:
        config = load_config()
    corp_id = config.get("dingtalk", {}).get("corp_id", "")
    if corp_id:
        params["corpId"] = corp_id
        params["scope"] = "openid corpid"

    return "https://login.dingtalk.com/oauth2/auth?" + urllib.parse.urlencode(params)


# ---------------------------------------------------------------------------
# unionId 解析（消费个人 token，token 不持久化）
# ---------------------------------------------------------------------------

def resolve_unionid(config: dict | None, user_id: str, access_token: str) -> dict:
    """用个人 access token 调 /contact/users/me 获取 unionId 并存入映射。

    个人 token 仅在此处消费一次，不持久化。
    """
    resp = requests.get(
        f"{BASE_URL}/v1.0/contact/users/me",
        headers={"x-acs-dingtalk-access-token": access_token},
        timeout=10,
    )
    if not resp.ok:
        raise RuntimeError(f"Failed to get user info: {resp.status_code} {resp.text}")

    info = resp.json()
    union_id = info.get("unionId", "")
    if not union_id:
        raise RuntimeError(f"unionId not found in response: {info}")

    save_user_credentials(user_id, "dingtalk", {"union_id": union_id})

    return {
        "userId": user_id,
        "unionId": union_id,
        "nick": info.get("nick", ""),
    }


# ---------------------------------------------------------------------------
# 共享 operatorId 解析（供 dingtalk_doc / sheet / wiki 调用）
# ---------------------------------------------------------------------------

def require_operator_id(config, args, scope_label: str = "钉钉文档") -> str:
    """按优先级解析 operatorId: proxy > env > --operator-id > --user-id 查映射。

    代理模式下返回占位符（Bot 侧代理注入真实 operatorId）。
    当 --user-id 映射未命中时，输出授权链接+指引文案后 exit(0)（两轮对话模式）。
    scope_label 用于提示文案，如 "钉钉文档" / "钉钉表格" / "钉钉知识库"。
    """
    if os.environ.get("_TYCLAW_DT_PROXY_URL"):
        return "__proxy__"

    env_uid = os.environ.get("_TYCLAW_DT_UNION_ID", "")
    if env_uid:
        return env_uid

    oid = getattr(args, "operator_id", None)
    if oid:
        return oid

    user_id = getattr(args, "user_id", None)
    if user_id:
        union_id = get_unionid(user_id)
        if union_id:
            return union_id
        try:
            auth_url = build_auth_url(config, user_id)
        except ValueError as e:
            print(f"Error: {e}", file=sys.stderr)
            sys.exit(1)
        print(f"你还没有授权{scope_label}操作权限，请点击下方链接完成一次性授权：\n\n"
              f"[点击授权]({auth_url})\n\n"
              f"完成授权后回到对话中告诉我「继续」或重新描述你的需求，我会接着帮你完成操作。")
        sys.exit(0)

    print("Error: --operator-id or --user-id is required", file=sys.stderr)
    sys.exit(1)


# ---------------------------------------------------------------------------
# CLI 子命令
# ---------------------------------------------------------------------------

def cmd_check(_config, args):
    union_id = get_unionid(args.user_id)
    print(format_json({
        "userId": args.user_id,
        "unionId": union_id,
        "found": union_id is not None,
    }))


def cmd_auth_url(config, args):
    try:
        url = build_auth_url(config, args.user_id)
    except ValueError as e:
        print(f"Error: {e}", file=sys.stderr)
        sys.exit(1)
    print(url)


def cmd_resolve(config, args):
    try:
        result = resolve_unionid(config, args.user_id, args.access_token)
    except RuntimeError as e:
        print(f"Error: {e}", file=sys.stderr)
        sys.exit(1)
    print(format_json(result))


# ---------------------------------------------------------------------------
# argparse
# ---------------------------------------------------------------------------

def main():
    parser = argparse.ArgumentParser(description="钉钉 OAuth 授权与 unionId 映射管理")
    parser.add_argument("--config", help="配置文件路径")
    sub = parser.add_subparsers(dest="command", required=True)

    p = sub.add_parser("check", help="检查 userId 是否已有 unionId 映射")
    p.add_argument("--user-id", required=True, help="用户 userId")

    p = sub.add_parser("auth-url", help="生成 OAuth 授权 URL")
    p.add_argument("--user-id", required=True, help="用户 userId（写入 state 参数）")

    p = sub.add_parser("resolve", help="用个人 access token 获取 unionId 并写入映射")
    p.add_argument("--user-id", required=True, help="用户 userId")
    p.add_argument("--access-token", required=True, help="用户 OAuth access token（用完即丢）")

    args = parser.parse_args()
    config = load_config(getattr(args, "config", None))

    dispatch = {
        "check": cmd_check,
        "auth-url": cmd_auth_url,
        "resolve": cmd_resolve,
    }
    dispatch[args.command](config, args)


if __name__ == "__main__":
    main()
