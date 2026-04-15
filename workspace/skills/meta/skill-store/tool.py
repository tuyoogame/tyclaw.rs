"""
Skill 商店（统一目录）
浏览、搜索、安装、卸载 builtin optional + shared Skill，展示 personal Skill

用法:
  python3 skills/skill-store/tool.py list [--keyword <kw>]
  python3 skills/skill-store/tool.py install <skill_key>
  python3 skills/skill-store/tool.py uninstall <skill_key>
"""

import argparse
import json
import os
import re
import sys

import yaml


# ── helpers ──────────────────────────────────────────────────────────

def _parse_frontmatter(content: str) -> dict:
    m = re.match(r"^---\s*\n(.*?)\n---\s*\n", content, re.DOTALL)
    if not m:
        return {}
    try:
        return yaml.safe_load(m.group(1)) or {}
    except yaml.YAMLError:
        return {}


def _personal_dir() -> str:
    d = os.environ.get("TYCLAW_PERSONAL_DIR", "")
    if not d:
        print("Error: TYCLAW_PERSONAL_DIR not set", file=sys.stderr)
        sys.exit(1)
    return d


# ── builtin skills scan ─────────────────────────────────────────────

def _scan_builtin_skills() -> list[dict]:
    skills_dir = os.path.join(os.path.dirname(__file__), "..")
    skills_dir = os.path.normpath(skills_dir)
    results = []
    for name in sorted(os.listdir(skills_dir)):
        skill_dir = os.path.join(skills_dir, name)
        if not os.path.isdir(skill_dir):
            continue
        md_path = os.path.join(skill_dir, "SKILL.md")
        if not os.path.exists(md_path):
            continue
        try:
            with open(md_path, "r", encoding="utf-8") as f:
                content = f.read()
        except OSError:
            continue
        meta = _parse_frontmatter(content)
        if not meta.get("name"):
            continue
        results.append({
            "key": name,
            "name": meta["name"],
            "description": meta.get("description", ""),
            "default": bool(meta.get("default", False)),
        })
    return results


# ── personal skills scan ────────────────────────────────────────────

def _scan_personal_skills() -> list[dict]:
    pdir = _personal_dir()
    skills_dir = os.path.join(pdir, "skills")
    if not os.path.isdir(skills_dir):
        return []
    results = []
    for name in sorted(os.listdir(skills_dir)):
        skill_dir = os.path.join(skills_dir, name)
        if not os.path.isdir(skill_dir):
            continue
        md_path = os.path.join(skill_dir, "SKILL.md")
        if not os.path.exists(md_path):
            continue
        try:
            with open(md_path, "r", encoding="utf-8") as f:
                content = f.read()
        except OSError:
            continue
        meta = _parse_frontmatter(content)
        results.append({
            "key": name,
            "name": meta.get("name", name),
            "description": meta.get("description", ""),
        })
    return results


# ── JSON file helpers ───────────────────────────────────────────────

def _load_json(path: str) -> list | dict:
    if not os.path.exists(path):
        return [] if path.endswith(".json") else {}
    try:
        with open(path, "r", encoding="utf-8") as f:
            return json.load(f)
    except (json.JSONDecodeError, OSError):
        return [] if path.endswith(".json") else {}


def _builtin_installed_path() -> str:
    return os.path.join(_personal_dir(), "installed_skills.json")


def _shared_installed_path() -> str:
    return os.path.join(_personal_dir(), "installed_shared_skills.json")


def _discoverable_path() -> str:
    return os.path.join(_personal_dir(), ".discoverable_shared.json")


def _store_stats_path() -> str:
    return os.path.join(_personal_dir(), ".skill_store_stats.json")


def _load_list(path: str) -> list[str]:
    data = _load_json(path)
    return data if isinstance(data, list) else []


def _save_list(path: str, keys: list[str]):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    tmp = path + ".tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(keys, f, ensure_ascii=False)
    os.replace(tmp, path)


# ── stats formatting ────────────────────────────────────────────────

def _fmt_stats(installed_count: int, usage_count: int) -> str:
    parts = []
    parts.append(f"{installed_count}人安装")
    parts.append(f"{usage_count}次使用")
    return " · ".join(parts)


def _match_keyword(keyword: str, *fields: str) -> bool:
    if not keyword:
        return True
    kw = keyword.lower()
    return any(kw in (f or "").lower() for f in fields)


# ── subcommands ──────────────────────────────────────────────────────

def cmd_list(args):
    pdir = _personal_dir()
    keyword = (args.keyword or "").strip()

    all_builtin = _scan_builtin_skills()
    builtin_installed = set(_load_list(_builtin_installed_path()))
    shared_installed_keys = set(_load_list(_shared_installed_path()))
    discoverable = _load_json(_discoverable_path())
    if not isinstance(discoverable, list):
        discoverable = []
    store_stats = _load_json(_store_stats_path())
    if not isinstance(store_stats, dict):
        store_stats = {}
    builtin_stats = store_stats.get("builtin", {})
    shared_inst_stats = store_stats.get("shared_installed", {})

    personal = _scan_personal_skills()

    # ── 分类 ──
    default_skills = [s for s in all_builtin if s["default"]]
    optional_installed = [s for s in all_builtin
                          if not s["default"] and s["key"] in builtin_installed]
    optional_available = [s for s in all_builtin
                          if not s["default"] and s["key"] not in builtin_installed]

    # 已安装的 shared skills（从 discoverable + store_stats 取元数据）
    shared_installed_list = []
    for key in sorted(shared_installed_keys):
        st = shared_inst_stats.get(key, {})
        shared_installed_list.append({
            "key": key,
            "display_name": st.get("display_name", key.split("--", 1)[-1] if "--" in key else key),
            "author_name": st.get("author_name", "?"),
            "installed_count": st.get("installed_count", 0),
            "usage_count": st.get("usage_count", 0),
        })

    # 可安装的 shared skills（按安装数降序）
    shared_available = sorted(discoverable,
                              key=lambda e: e.get("installed_count", 0),
                              reverse=True)

    has_output = False
    print("## Skill 目录\n")

    # ── 我的 Skill ──
    filtered_personal = [s for s in personal
                         if _match_keyword(keyword, s["name"], s["description"], s["key"])]
    if filtered_personal:
        print(f"### 我的 Skill（{len(filtered_personal)} 个）\n")
        for s in filtered_personal:
            print(f"- **{s['name']}** (key: `{s['key']}`)：{s['description']}")
        print()
        has_output = True

    # ── 已安装 ──
    installed_lines = []
    for s in optional_installed:
        if not _match_keyword(keyword, s["name"], s["description"], s["key"]):
            continue
        st = builtin_stats.get(s["key"], {})
        stats_str = _fmt_stats(st.get("installed_count", 0), st.get("usage_count", 0))
        installed_lines.append(
            f"- **{s['name']}** (key: `{s['key']}`) | {stats_str}")

    for s in shared_installed_list:
        if not _match_keyword(keyword, s["display_name"], s["key"], s["author_name"]):
            continue
        stats_str = _fmt_stats(s["installed_count"], s["usage_count"])
        installed_lines.append(
            f"- **{s['display_name']}** (key: `{s['key']}`, by {s['author_name']}) | {stats_str}")

    if installed_lines:
        print(f"### 已安装（{len(installed_lines)} 个，可卸载）\n")
        for line in installed_lines:
            print(line)
        print()
        has_output = True

    # ── 可安装 — 系统 Skill ──
    avail_builtin_lines = []
    for s in optional_available:
        if not _match_keyword(keyword, s["name"], s["description"], s["key"]):
            continue
        st = builtin_stats.get(s["key"], {})
        stats_str = _fmt_stats(st.get("installed_count", 0), st.get("usage_count", 0))
        avail_builtin_lines.append(
            f"- **{s['name']}** (key: `{s['key']}`) | {stats_str}：{s['description']}")

    if avail_builtin_lines:
        print("### 可安装 — 系统 Skill\n")
        for line in avail_builtin_lines:
            print(line)
        print()
        has_output = True

    # ── 可安装 — 共享 Skill（按安装数排序）──
    avail_shared_lines = []
    for s in shared_available:
        if not _match_keyword(keyword, s.get("display_name", ""),
                              s.get("description", ""),
                              s.get("author_name", ""), s.get("key", "")):
            continue
        stats_str = _fmt_stats(s.get("installed_count", 0), s.get("usage_count", 0))
        display = s.get("display_name") or s.get("skill_name", "?")
        avail_shared_lines.append(
            f"- **{display}** (key: `{s['key']}`, by {s.get('author_name', '?')}) "
            f"| {stats_str}：{s.get('description', '')}")

    if avail_shared_lines:
        print("### 可安装 — 共享 Skill（按安装数排序）\n")
        for line in avail_shared_lines:
            print(line)
        print()
        has_output = True

    if not has_output and keyword:
        print(f"没有找到匹配「{keyword}」的 Skill。\n")

    print("---")
    print("安装：`python3 skills/skill-store/tool.py install <key>`")
    print("卸载：`python3 skills/skill-store/tool.py uninstall <key>`")


def cmd_install(args):
    key = args.skill_key
    is_shared = "--" in key

    if is_shared:
        _install_shared(key)
    else:
        _install_builtin(key)


def cmd_uninstall(args):
    key = args.skill_key
    is_shared = "--" in key

    if is_shared:
        _uninstall_shared(key)
    else:
        _uninstall_builtin(key)


# ── builtin install/uninstall ────────────────────────────────────────

def _install_builtin(key: str):
    all_skills = _scan_builtin_skills()
    skill_map = {s["key"]: s for s in all_skills}

    if key not in skill_map:
        print(f"Error: unknown skill key '{key}'")
        print(f"Available keys: {', '.join(s['key'] for s in all_skills if not s['default'])}")
        sys.exit(1)

    skill = skill_map[key]
    if skill["default"]:
        print(f"「{skill['name']}」是默认 Skill，已自动启用，无需安装。")
        return

    path = _builtin_installed_path()
    installed = _load_list(path)
    if key in installed:
        print(f"「{skill['name']}」已经安装过了。")
        return

    installed.append(key)
    _save_list(path, installed)
    print(f"已安装「{skill['name']}」，现在可以使用了。")


def _uninstall_builtin(key: str):
    all_skills = _scan_builtin_skills()
    skill_map = {s["key"]: s for s in all_skills}

    if key not in skill_map:
        print(f"Error: unknown skill key '{key}'")
        sys.exit(1)

    skill = skill_map[key]
    if skill["default"]:
        print(f"「{skill['name']}」是系统核心 Skill，无法卸载。")
        return

    path = _builtin_installed_path()
    installed = _load_list(path)
    if key not in installed:
        print(f"「{skill['name']}」尚未安装。")
        return

    installed.remove(key)
    _save_list(path, installed)
    print(f"已卸载「{skill['name']}」。")


# ── shared install/uninstall ─────────────────────────────────────────

def _install_shared(shared_key: str):
    discoverable = _load_json(_discoverable_path())
    if not isinstance(discoverable, list):
        discoverable = []
    entry = next((e for e in discoverable if e.get("key") == shared_key), None)
    display = entry.get("display_name", shared_key) if entry else shared_key

    path = _shared_installed_path()
    installed = _load_list(path)
    if shared_key in installed:
        print(f"「{display}」已经安装过了。")
        return

    installed.append(shared_key)
    _save_list(path, installed)

    author = entry.get("author_name", "?") if entry else "?"
    print(f"已安装共享 Skill「{display}」（by {author}），下次对话即可使用。")


def _uninstall_shared(shared_key: str):
    path = _shared_installed_path()
    installed = _load_list(path)

    if shared_key not in installed:
        print(f"共享 Skill「{shared_key}」尚未安装。")
        return

    installed.remove(shared_key)
    _save_list(path, installed)
    print(f"已卸载共享 Skill「{shared_key}」。")


# ── main ─────────────────────────────────────────────────────────────

def main():
    parser = argparse.ArgumentParser(description="Skill 商店")
    sub = parser.add_subparsers(dest="command")

    p_list = sub.add_parser("list", help="List all skills")
    p_list.add_argument("--keyword", default="", help="Search keyword")

    p_install = sub.add_parser("install", help="Install a skill")
    p_install.add_argument("skill_key", help="Skill key to install")

    p_uninstall = sub.add_parser("uninstall", help="Uninstall a skill")
    p_uninstall.add_argument("skill_key", help="Skill key to uninstall")

    args = parser.parse_args()
    if args.command == "list":
        cmd_list(args)
    elif args.command == "install":
        cmd_install(args)
    elif args.command == "uninstall":
        cmd_uninstall(args)
    else:
        parser.print_help()
        sys.exit(1)


if __name__ == "__main__":
    main()
