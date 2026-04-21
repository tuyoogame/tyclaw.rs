#!/usr/bin/env python3
"""Discord 社区数据工具
直接调用 Discord REST API，凭证通过环境变量注入（_TYCLAW_DC_BOT_TOKEN / _TYCLAW_DC_GUILD_ID）。
"""

import argparse
import json
import os
import sys

import requests

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
from utils import (save_user_credentials, sync_credential_env,
                    clear_user_credentials, clear_credential_env,
                    get_injected_credential)

API_BASE = "https://discord.com/api/v10"
_TIMEOUT = 30


def _get_token() -> str:
    token = get_injected_credential("discord", "bot_token")
    if not token:
        print("Error: Discord bot_token not configured. "
              "Run: python3 tools/discord_api.py setup --bot-token YOUR_TOKEN --guild-id YOUR_GUILD_ID",
              file=sys.stderr)
        sys.exit(1)
    return token


def _get_guild_id(override: str | None = None) -> str:
    gid = override or get_injected_credential("discord", "guild_id") or ""
    if not gid:
        print("Error: Discord guild_id not configured. "
              "Run: python3 tools/discord_api.py setup --guild-id YOUR_GUILD_ID",
              file=sys.stderr)
        sys.exit(1)
    return gid


def _headers(token: str) -> dict:
    return {"Authorization": f"Bot {token}", "Content-Type": "application/json"}


def _api_get(token: str, path: str, params: dict | None = None) -> dict | list:
    resp = requests.get(f"{API_BASE}{path}", headers=_headers(token),
                        params=params, timeout=_TIMEOUT)
    if resp.status_code != 200:
        try:
            msg = resp.json().get("message", resp.text[:500])
        except Exception:
            msg = resp.text[:500]
        print(f"Error: Discord API {resp.status_code}: {msg}", file=sys.stderr)
        sys.exit(1)
    return resp.json()


# ── Commands ──

def _cmd_setup(args):
    staff_id = os.environ.get("TYCLAW_SENDER_STAFF_ID", "")
    if not staff_id:
        print("Error: TYCLAW_SENDER_STAFF_ID env var required", file=sys.stderr)
        sys.exit(1)
    data = {}
    if args.bot_token:
        data["bot_token"] = args.bot_token
    if args.guild_id:
        data["guild_id"] = args.guild_id
    if not data:
        print("Error: at least one of --bot-token or --guild-id required",
              file=sys.stderr)
        sys.exit(1)
    save_user_credentials(staff_id, "discord", data)
    sync_credential_env("discord", data)
    print(f"Discord credentials saved for {staff_id}")


def _cmd_clear_credentials(_args):
    staff_id = os.environ.get("TYCLAW_SENDER_STAFF_ID", "")
    if not staff_id:
        print("Error: TYCLAW_SENDER_STAFF_ID env var required", file=sys.stderr)
        sys.exit(1)
    if clear_user_credentials(staff_id, "discord"):
        clear_credential_env("discord")
        print(f"Discord credentials cleared for {staff_id}")
    else:
        print(f"No Discord credentials found for {staff_id}")


def _cmd_get_guild(args):
    token = _get_token()
    gid = _get_guild_id(args.guild_id)
    data = _api_get(token, f"/guilds/{gid}", {"with_counts": "true"})
    result = {
        "id": data.get("id"),
        "name": data.get("name"),
        "description": data.get("description"),
        "member_count": data.get("approximate_member_count"),
        "online_count": data.get("approximate_presence_count"),
        "owner_id": data.get("owner_id"),
        "premium_tier": data.get("premium_tier"),
        "roles": [{"id": r["id"], "name": r["name"], "position": r["position"],
                    "color": r["color"], "managed": r.get("managed", False)}
                   for r in data.get("roles", [])],
    }
    print(json.dumps(result, ensure_ascii=False, indent=2))


def _cmd_list_channels(args):
    token = _get_token()
    gid = _get_guild_id(args.guild_id)
    data = _api_get(token, f"/guilds/{gid}/channels")
    type_names = {0: "text", 2: "voice", 4: "category", 5: "announcement",
                  13: "stage", 15: "forum"}
    channels = []
    for ch in data:
        if args.type is not None and ch.get("type") != args.type:
            continue
        channels.append({
            "id": ch.get("id"),
            "name": ch.get("name"),
            "type": ch.get("type"),
            "type_name": type_names.get(ch.get("type"), "unknown"),
            "parent_id": ch.get("parent_id"),
            "topic": ch.get("topic"),
            "position": ch.get("position"),
        })
    channels.sort(key=lambda c: (c.get("position", 0), c["id"]))
    print(json.dumps({"total": len(channels), "channels": channels},
                     ensure_ascii=False, indent=2))


def _cmd_get_messages(args):
    token = _get_token()
    query: dict = {"limit": min(args.limit, 100)}
    if args.before:
        query["before"] = args.before
    if args.after:
        query["after"] = args.after
    if args.around:
        query["around"] = args.around
    data = _api_get(token, f"/channels/{args.channel_id}/messages", query)
    messages = []
    for m in data:
        msg = {
            "id": m.get("id"),
            "author_id": m.get("author", {}).get("id"),
            "author_name": m.get("author", {}).get("username"),
            "author_bot": m.get("author", {}).get("bot", False),
            "content": m.get("content", ""),
            "timestamp": m.get("timestamp"),
            "type": m.get("type", 0),
            "attachments": len(m.get("attachments", [])),
            "embeds": len(m.get("embeds", [])),
            "reactions": [{"emoji": r["emoji"].get("name", ""),
                           "count": r.get("count", 0)}
                          for r in m.get("reactions", [])],
        }
        if m.get("referenced_message"):
            ref = m["referenced_message"]
            msg["reply_to"] = {
                "id": ref.get("id"),
                "author_name": ref.get("author", {}).get("username"),
                "content": (ref.get("content", "") or "")[:100],
            }
        if m.get("thread"):
            msg["thread_id"] = m["thread"]["id"]
            msg["thread_name"] = m["thread"].get("name", "")
        messages.append(msg)
    print(json.dumps({"total": len(messages), "messages": messages},
                     ensure_ascii=False, indent=2))


def _cmd_list_members(args):
    token = _get_token()
    gid = _get_guild_id(args.guild_id)
    query: dict = {"limit": min(args.limit, 1000)}
    if args.after:
        query["after"] = args.after
    data = _api_get(token, f"/guilds/{gid}/members", query)
    members = []
    for m in data:
        user = m.get("user", {})
        members.append({
            "user_id": user.get("id"),
            "username": user.get("username"),
            "display_name": m.get("nick") or user.get("global_name") or user.get("username"),
            "bot": user.get("bot", False),
            "joined_at": m.get("joined_at"),
            "roles": m.get("roles", []),
        })
    print(json.dumps({"total": len(members), "members": members},
                     ensure_ascii=False, indent=2))


def _cmd_search_members(args):
    token = _get_token()
    gid = _get_guild_id(args.guild_id)
    query: dict = {"query": args.query, "limit": min(args.limit, 1000)}
    data = _api_get(token, f"/guilds/{gid}/members/search", query)
    members = []
    for m in data:
        user = m.get("user", {})
        members.append({
            "user_id": user.get("id"),
            "username": user.get("username"),
            "display_name": m.get("nick") or user.get("global_name") or user.get("username"),
            "bot": user.get("bot", False),
            "joined_at": m.get("joined_at"),
            "roles": m.get("roles", []),
        })
    print(json.dumps({"total": len(members), "members": members},
                     ensure_ascii=False, indent=2))


def _cmd_list_threads(args):
    token = _get_token()
    gid = _get_guild_id(args.guild_id)
    data = _api_get(token, f"/guilds/{gid}/threads/active")
    threads = []
    for t in data.get("threads", []):
        threads.append({
            "id": t.get("id"),
            "name": t.get("name"),
            "parent_id": t.get("parent_id"),
            "owner_id": t.get("owner_id"),
            "message_count": t.get("message_count"),
            "member_count": t.get("member_count"),
            "type": t.get("type"),
        })
    print(json.dumps({"total": len(threads), "threads": threads},
                     ensure_ascii=False, indent=2))


def main():
    parser = argparse.ArgumentParser(description="Discord community data tool")
    sub = parser.add_subparsers(dest="action", required=True)

    # ── setup / clear ──
    p_setup = sub.add_parser("setup", help="Set Discord Bot Token and Guild ID")
    p_setup.add_argument("--bot-token", help="Discord Bot Token")
    p_setup.add_argument("--guild-id", help="Discord Guild (server) ID")
    sub.add_parser("clear-credentials", help="Clear Discord credentials")

    # ── get-guild ──
    p_guild = sub.add_parser("get-guild", help="Get guild (server) info")
    p_guild.add_argument("--guild-id", help="Override default guild ID")

    # ── list-channels ──
    p_ch = sub.add_parser("list-channels", help="List guild channels")
    p_ch.add_argument("--guild-id", help="Override default guild ID")
    p_ch.add_argument("--type", type=int, choices=[0, 2, 4, 5, 13, 15],
                      help="Filter by type: 0=text 2=voice 4=category 5=announcement 15=forum")

    # ── get-messages ──
    p_msg = sub.add_parser("get-messages", help="Get channel messages")
    p_msg.add_argument("--channel-id", required=True, help="Channel or thread ID")
    p_msg.add_argument("--limit", type=int, default=50,
                       help="Number of messages (max 100)")
    p_msg.add_argument("--before", help="Get messages before this message ID")
    p_msg.add_argument("--after", help="Get messages after this message ID")
    p_msg.add_argument("--around", help="Get messages around this message ID")

    # ── list-members ──
    p_mem = sub.add_parser("list-members", help="List guild members")
    p_mem.add_argument("--guild-id", help="Override default guild ID")
    p_mem.add_argument("--limit", type=int, default=100,
                       help="Number of members (max 1000)")
    p_mem.add_argument("--after", help="Get members after this user ID (pagination)")

    # ── search-members ──
    p_smem = sub.add_parser("search-members", help="Search guild members by name")
    p_smem.add_argument("--guild-id", help="Override default guild ID")
    p_smem.add_argument("--query", required=True, help="Username/nickname to search")
    p_smem.add_argument("--limit", type=int, default=10, help="Max results")

    # ── list-threads ──
    p_th = sub.add_parser("list-threads", help="List active threads in guild")
    p_th.add_argument("--guild-id", help="Override default guild ID")

    args = parser.parse_args()

    dispatch = {
        "setup": _cmd_setup,
        "clear-credentials": _cmd_clear_credentials,
        "get-guild": _cmd_get_guild,
        "list-channels": _cmd_list_channels,
        "get-messages": _cmd_get_messages,
        "list-members": _cmd_list_members,
        "search-members": _cmd_search_members,
        "list-threads": _cmd_list_threads,
    }
    dispatch[args.action](args)


if __name__ == "__main__":
    main()
