"""
用户凭证总览工具
查看所有凭证状态、清除指定凭证

凭证设置已迁移到各 Skill 的 tool.py（setup 子命令），本工具仅负责只读展示和清除。

用法:
  python tools/user_settings.py show
  python tools/user_settings.py clear --section ga
"""

import argparse
import os
import re
import sys
import time

import yaml

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
from utils import (
    load_user_credentials, clear_user_credentials,
    check_td_token_expiry, clear_credential_env,
)


def _mask(value: str, keep: int = 3) -> str:
    if not value:
        return "(未设置)"
    if len(value) <= keep * 2:
        return "*" * len(value)
    return value[:keep] + "*" * (len(value) - keep * 2) + value[-keep:]


def _get_staff_id() -> str:
    staff_id = os.environ.get("TYCLAW_SENDER_STAFF_ID", "")
    if not staff_id:
        print("Error: TYCLAW_SENDER_STAFF_ID env var required",
              file=sys.stderr)
        sys.exit(1)
    return staff_id


# ---------------------------------------------------------------------------
# 动态发现 Skill 声明的 credentials 元数据
# ---------------------------------------------------------------------------

def _parse_frontmatter(content: str) -> dict:
    m = re.match(r"^---\s*\n(.*?)\n---\s*\n", content, re.DOTALL)
    if not m:
        return {}
    try:
        return yaml.safe_load(m.group(1)) or {}
    except yaml.YAMLError:
        return {}


def _discover_credential_specs() -> list[dict]:
    """扫描 skills/ 目录下所有 SKILL.md，收集 credentials 声明。

    返回 [{"key": ..., "display_name": ..., "setup": ..., "fields": [...], "skill_name": ...}, ...]
    """
    skills_dir = os.path.normpath(os.path.join(os.path.dirname(__file__), "..", "skills"))
    specs = []
    if not os.path.isdir(skills_dir):
        return specs
    for name in sorted(os.listdir(skills_dir)):
        md_path = os.path.join(skills_dir, name, "SKILL.md")
        if not os.path.isfile(md_path):
            continue
        try:
            with open(md_path, "r", encoding="utf-8") as f:
                content = f.read()
        except OSError:
            continue
        meta = _parse_frontmatter(content)
        cred = meta.get("credentials")
        if not cred or not isinstance(cred, dict):
            continue
        cred_key = cred.get("key", "")
        if not cred_key:
            continue
        specs.append({
            "key": cred_key,
            "display_name": cred.get("display_name", cred_key),
            "setup": cred.get("setup", "manual"),
            "fields": cred.get("fields", []),
            "skill_name": meta.get("name", name),
            "skill_key": name,
        })
    return specs


# ---------------------------------------------------------------------------
# 各类凭证的展示逻辑
# ---------------------------------------------------------------------------

def _render_credential(spec: dict, section_data: dict) -> list[str]:
    """根据 spec 和实际数据，生成展示行。"""
    display = spec["display_name"]
    setup = spec["setup"]
    fields = spec.get("fields", [])
    lines = []

    if not section_data:
        setup_hint = {
            "manual": f"请发送「设置{display}凭证」进行配置",
            "token": f"请发送「设置{display}凭证」进行配置",
            "oauth": f"使用对应功能时会自动引导授权",
        }.get(setup, "请查看对应 Skill 文档")
        lines.append(f"**{display}凭证** (未配置)")
        lines.append(f"- {setup_hint}")
        return lines

    lines.append(f"**{display}凭证** (已配置)")

    # 特殊处理：TD token 过期检查
    if spec["key"] == "td":
        token = section_data.get("token", "")
        lines.append(f"- token: {_mask(token)}")
        days, expire_time = check_td_token_expiry(token)
        if days is not None:
            if days <= 0:
                lines.append(f"- 状态: ⚠️ **已过期**（{expire_time}）")
            elif days <= 2:
                lines.append(f"- 状态: ⚠️ 即将过期（{expire_time}，"
                             f"剩余 {days * 24:.0f} 小时）")
            else:
                lines.append(f"- 过期时间: {expire_time}（剩余 {days:.1f} 天）")
        return lines

    # 特殊处理：微信公众号
    if spec["key"] == "wechat":
        nickname = section_data.get("nickname", "(未知)")
        lines.append(f"- 公众号: {nickname}")
        lines.append(f"- token: {_mask(section_data.get('token', ''))}")
        expire_ms = section_data.get("expire_time", "")
        if expire_ms:
            try:
                remain_s = int(expire_ms) / 1000 - time.time()
                if remain_s <= 0:
                    lines.append("- 状态: ⚠️ **已过期**，请重新扫码授权")
                else:
                    hours = remain_s / 3600
                    if hours < 48:
                        lines.append(f"- 状态: ⚠️ 即将过期（剩余 {hours:.0f} 小时）")
                    else:
                        lines.append(f"- 过期时间: 剩余 {hours / 24:.1f} 天")
            except (ValueError, TypeError):
                pass
        lines.append("- 设置方式: 通过扫码授权（非手动设置）")
        return lines

    # 通用字段展示
    for field in fields:
        fname = field.get("name", "")
        val = section_data.get(fname, "")
        if field.get("secret"):
            lines.append(f"- {fname}: {_mask(val)}")
        else:
            lines.append(f"- {fname}: {val or '(未设置)'}")

    return lines


def cmd_show(_args):
    staff_id = _get_staff_id()
    creds = load_user_credentials(staff_id)
    specs = _discover_credential_specs()

    lines = ["## 当前凭证配置\n"]
    rendered_keys = set()

    for spec in specs:
        key = spec["key"]
        rendered_keys.add(key)
        section_data = creds.get(key, {})
        lines.extend(_render_credential(spec, section_data))
        lines.append("")

    # Fallback：credentials.yaml 中存在但没有匹配到任何 Skill 的 section
    internal_keys = {"dingtalk"}  # 系统内部用，不展示
    for key, data in creds.items():
        if key in rendered_keys or key in internal_keys or not isinstance(data, dict):
            continue
        lines.append(f"**{key}凭证** (已配置，对应 Skill 未安装)")
        for fname, val in data.items():
            lines.append(f"- {fname}: {_mask(str(val)) if 'password' in fname or 'token' in fname or 'secret' in fname or 'cookie' in fname else val}")
        lines.append("")

    print("\n".join(lines))


def cmd_clear(args):
    staff_id = _get_staff_id()
    section = args.section
    if clear_user_credentials(staff_id, section):
        clear_credential_env(section)
        print(f"{section.upper()} credentials cleared for {staff_id}")
    else:
        print(f"No {section.upper()} credentials found for {staff_id}")


def main():
    parser = argparse.ArgumentParser(description="User credentials dashboard")
    sub = parser.add_subparsers(dest="command")

    sub.add_parser("show", help="Show all credentials status (masked)")

    p_clear = sub.add_parser("clear", help="Clear credentials for a section")
    p_clear.add_argument("--section", required=True,
                         help="Credential section key (e.g. ga, td, email, adx, cl, wechat)")

    args = parser.parse_args()

    if args.command == "show":
        cmd_show(args)
    elif args.command == "clear":
        cmd_clear(args)
    else:
        parser.print_help()
        sys.exit(1)


if __name__ == "__main__":
    main()
