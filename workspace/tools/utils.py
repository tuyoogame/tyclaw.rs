"""
公共工具模块
提供配置读取、输出格式化等通用功能
"""

import hashlib
import json
import os
import sys
import time
from pathlib import Path

import yaml

_PROJECT_ROOT = Path(__file__).resolve().parent.parent
if str(_PROJECT_ROOT) not in sys.path:
    sys.path.insert(0, str(_PROJECT_ROOT))


def get_project_root():
    """获取项目根目录（TyClaw/）"""
    return _PROJECT_ROOT


def workspace_path(works_dir: str | Path, key: str) -> Path:
    """计算 workspace 路径：{works_dir}/{md5(key)[0]:02x}/{key}，与 Rust 版本一致"""
    bucket = f"{hashlib.md5(key.encode()).digest()[0]:02x}"
    return Path(works_dir) / bucket / key


_works_dir_cache: Path | None = None


def init_works_dir(config: dict):
    """启动时调用一次，从 config 解析 works_dir 并缓存，后续 get_works_dir() 无需传参。"""
    global _works_dir_cache
    raw = config.get("works_dir", "")
    _works_dir_cache = Path(raw) if raw else _PROJECT_ROOT / "works"


def get_works_dir() -> Path:
    if _works_dir_cache is not None:
        return _works_dir_cache
    return _PROJECT_ROOT / "works"


def load_config(config_path=None):
    """
    读取 config.yaml 配置文件

    Args:
        config_path: 配置文件路径，默认为 config/config.yaml
    Returns:
        dict: 配置字典
    """
    if config_path is None:
        config_path = get_project_root() / "config" / "config.yaml"
    else:
        config_path = Path(config_path)

    if not config_path.exists():
        return {}

    with open(config_path, "r", encoding="utf-8") as f:
        config = yaml.safe_load(f)

    return config


def _get_user_credentials_path(staff_id: str) -> Path:
    personal_dir = os.environ.get("TYCLAW_PERSONAL_DIR", "")
    if personal_dir:
        return Path(personal_dir) / "credentials.yaml"
    return workspace_path(get_works_dir(), staff_id) / "credentials.yaml"


def load_user_config(staff_id: str = "", config_path=None) -> dict:
    """加载配置：个人凭证覆盖全局配置。staff_id 为空时回退到环境变量。"""
    import copy
    config = copy.deepcopy(load_config(config_path))
    if not staff_id:
        staff_id = os.environ.get("TYCLAW_SENDER_STAFF_ID", "")
    if not staff_id:
        return config
    creds_path = _get_user_credentials_path(staff_id)
    if not creds_path.exists():
        return config
    try:
        with open(creds_path, "r", encoding="utf-8") as f:
            user_creds = yaml.safe_load(f) or {}
    except (yaml.YAMLError, OSError):
        return config
    for section in ("ga", "td", "email", "adx", "cl", "wechat", "qimai", "st"):
        if section in user_creds:
            config[section] = user_creds[section]
    return config


def save_user_credentials(staff_id: str, section: str, data: dict):
    """保存用户凭证到 workspace 目录的 credentials.yaml"""
    creds_path = _get_user_credentials_path(staff_id)
    creds_path.parent.mkdir(parents=True, exist_ok=True)
    existing = {}
    if creds_path.exists():
        try:
            with open(creds_path, "r", encoding="utf-8") as f:
                existing = yaml.safe_load(f) or {}
        except (yaml.YAMLError, OSError):
            existing = {}
    existing[section] = data
    with open(creds_path, "w", encoding="utf-8") as f:
        yaml.dump(existing, f, allow_unicode=True, default_flow_style=False)


def clear_user_credentials(staff_id: str, section: str) -> bool:
    """清除用户指定 section 的凭证，返回是否有变化"""
    creds_path = _get_user_credentials_path(staff_id)
    if not creds_path.exists():
        return False
    try:
        with open(creds_path, "r", encoding="utf-8") as f:
            existing = yaml.safe_load(f) or {}
    except (yaml.YAMLError, OSError):
        return False
    if section not in existing:
        return False
    del existing[section]
    if existing:
        with open(creds_path, "w", encoding="utf-8") as f:
            yaml.dump(existing, f, allow_unicode=True, default_flow_style=False)
    else:
        creds_path.unlink(missing_ok=True)
    return True


def load_user_credentials(staff_id: str) -> dict:
    """读取用户凭证文件，返回原始 dict"""
    creds_path = _get_user_credentials_path(staff_id)
    if not creds_path.exists():
        return {}
    try:
        with open(creds_path, "r", encoding="utf-8") as f:
            return yaml.safe_load(f) or {}
    except (yaml.YAMLError, OSError):
        return {}


def load_user_model(staff_id: str) -> str | None:
    """读取用户的模型偏好，未设置返回 None"""
    creds = load_user_credentials(staff_id)
    return creds.get("model") or None


def save_user_model(staff_id: str, model: str | None):
    """保存/清除用户的模型偏好（存于 credentials.yaml 顶层 model 字段）"""
    creds_path = _get_user_credentials_path(staff_id)
    creds_path.parent.mkdir(parents=True, exist_ok=True)
    existing = {}
    if creds_path.exists():
        try:
            with open(creds_path, "r", encoding="utf-8") as f:
                existing = yaml.safe_load(f) or {}
        except (yaml.YAMLError, OSError):
            existing = {}
    if model:
        existing["model"] = model
    else:
        existing.pop("model", None)
    if existing:
        with open(creds_path, "w", encoding="utf-8") as f:
            yaml.dump(existing, f, allow_unicode=True, default_flow_style=False)
    else:
        creds_path.unlink(missing_ok=True)


# --- 凭证注入（Bot → 环境变量 → Tool）---
# Bot 预解析凭证并注入 _TYCLAW_* 环境变量，工具优先读取，
# 使 AI 无法通过冒用 staff_id 访问其他用户的凭证。

_CRED_ENV_MAP = {
    "ga": {"username": "_TYCLAW_GA_USERNAME", "password": "_TYCLAW_GA_PASSWORD",
           "project_id": "_TYCLAW_GA_PROJECT_ID"},
    "td": {"token": "_TYCLAW_TD_TOKEN"},
    "email": {"address": "_TYCLAW_EMAIL_ADDRESS", "password": "_TYCLAW_EMAIL_PASSWORD"},
    "adx": {"email": "_TYCLAW_ADX_EMAIL", "password": "_TYCLAW_ADX_PASSWORD"},
    "cl": {"email": "_TYCLAW_CL_EMAIL", "password": "_TYCLAW_CL_PASSWORD"},
    "wechat": {"token": "_TYCLAW_WECHAT_TOKEN", "cookie": "_TYCLAW_WECHAT_COOKIE",
               "fakeid": "_TYCLAW_WECHAT_FAKEID", "nickname": "_TYCLAW_WECHAT_NICKNAME",
               "expire_time": "_TYCLAW_WECHAT_EXPIRE_TIME"},
    "qimai": {"email": "_TYCLAW_QIMAI_EMAIL", "password": "_TYCLAW_QIMAI_PASSWORD"},
    "st": {"token": "_TYCLAW_ST_TOKEN"},
}


def build_credential_env(staff_id: str) -> dict[str, str]:
    """Bot 侧：加载用户凭证，构建 _TYCLAW_* 环境变量字典。

    在 run_cursor_cli 启动前调用，注入到 Cursor CLI 进程环境中，
    使工具不再需要通过 staff_id 从文件系统读取凭证。
    """
    if not staff_id:
        return {}
    config = load_user_config(staff_id)
    env: dict[str, str] = {}
    for section, keys in _CRED_ENV_MAP.items():
        section_data = config.get(section, {})
        for field, env_name in keys.items():
            value = section_data.get(field, "")
            if value:
                env[env_name] = str(value)

    return env


def get_injected_credential(section: str, field: str) -> str | None:
    """工具侧：优先读 Bot 注入的环境变量，未命中则回退读 credentials.yaml。

    回退使用 TYCLAW_SENDER_STAFF_ID 定位用户，该变量由 Bot 在 CLI 启动时注入，
    子进程无法篡改父进程的值，因此安全性与环境变量注入模式一致。
    """
    env_name = _CRED_ENV_MAP.get(section, {}).get(field)
    if not env_name:
        return None
    val = os.environ.get(env_name)
    if val:
        return val
    staff_id = os.environ.get("TYCLAW_SENDER_STAFF_ID", "")
    if not staff_id:
        return None
    creds = load_user_credentials(staff_id)
    return creds.get(section, {}).get(field) or None


def sync_credential_env(section: str, data: dict) -> None:
    """保存凭证后同步更新 _TYCLAW_* 环境变量，使同一 CLI 会话内后续调用立即生效。"""
    for field, env_name in _CRED_ENV_MAP.get(section, {}).items():
        value = data.get(field, "")
        if value:
            os.environ[env_name] = str(value)
        else:
            os.environ.pop(env_name, None)


def clear_credential_env(section: str) -> None:
    """清除凭证后同步删除 _TYCLAW_* 环境变量。"""
    for env_name in _CRED_ENV_MAP.get(section, {}).values():
        os.environ.pop(env_name, None)


_dt_token_cache: dict = {"token": "", "expiry": 0.0}


def get_dingtalk_token(config=None) -> str:
    """获取钉钉应用 token（App Token），带内存缓存。

    代理模式下返回空串（Bot 侧代理自行获取 token）。
    直连模式下从 config 读取 client_id/secret 换取 token。
    """
    if os.environ.get("_TYCLAW_DT_PROXY_URL"):
        return ""

    if _dt_token_cache["token"] and time.time() < _dt_token_cache["expiry"]:
        return _dt_token_cache["token"]

    client_id = os.environ.get("_TYCLAW_DT_CLIENT_ID", "")
    client_secret = os.environ.get("_TYCLAW_DT_CLIENT_SECRET", "")

    if not client_id or not client_secret:
        if config is None:
            config = load_config()
        dt = config.get("dingtalk", {})
        client_id = client_id or dt.get("client_id", "")
        client_secret = client_secret or dt.get("client_secret", "")

    if not client_id or not client_secret:
        raise ValueError("DingTalk client_id/client_secret not configured")

    import requests
    resp = requests.post(
        "https://api.dingtalk.com/v1.0/oauth2/accessToken",
        json={"appKey": client_id, "appSecret": client_secret},
        timeout=10,
    )
    resp.raise_for_status()
    data = resp.json()
    _dt_token_cache["token"] = data["accessToken"]
    _dt_token_cache["expiry"] = time.time() + data.get("expireIn", 7200) - 60
    return _dt_token_cache["token"]


def format_json(data):
    """格式化输出 JSON"""
    return json.dumps(data, ensure_ascii=False, indent=2, default=str)


def format_markdown_table(headers, rows):
    """
    将数据格式化为 Markdown 表格

    Args:
        headers: 列名列表
        rows: 行数据列表（每行是一个列表或字典）
    Returns:
        str: Markdown 表格字符串
    """
    if not headers or not rows:
        return "No data."

    if rows and isinstance(rows[0], dict):
        rows = [[str(row.get(h, "")) for h in headers] for row in rows]
    else:
        rows = [[str(v) if v is not None else "" for v in row] for row in rows]

    headers_str = [str(h) if h is not None else "" for h in headers]

    lines = []
    lines.append("| " + " | ".join(headers_str) + " |")
    lines.append("| " + " | ".join(["---"] * len(headers_str)) + " |")
    for row in rows:
        lines.append("| " + " | ".join(row) + " |")

    return "\n".join(lines)


def make_tmp_dir(staff_id: str, name: str) -> Path:
    """生成隔离的临时输出目录并创建，格式：/tmp/tyclaw_{staff_id}_{timestamp}_{name}"""
    ts = int(time.time() * 1000)
    tmp = Path(f"/tmp/tyclaw_{staff_id}_{ts}_{name}")
    tmp.mkdir(parents=True, exist_ok=True)
    return tmp


def find_cjk_font(size: int = 18):
    """查找可用的中文字体，返回 PIL ImageFont 对象"""
    import os
    from PIL import ImageFont

    project_font = os.path.join(os.path.dirname(os.path.dirname(__file__)), "fonts", "msyh.ttf")
    candidates = [
        (project_font, 0),
    ]
    for path, idx in candidates:
        if os.path.exists(path):
            try:
                f = ImageFont.truetype(path, size=size, index=idx)
                if f.getbbox("测")[2] > 0:
                    return f
            except Exception:
                continue
    return ImageFont.load_default()


def check_td_token_expiry(token: str) -> tuple[float | None, str | None]:
    """解码 JWT token 提取过期信息，无需密钥。

    Returns:
        (days_remaining, expire_time_str) — 解码失败时返回 (None, None)
    """
    import datetime
    try:
        import jwt
        payload = jwt.decode(token, options={"verify_signature": False})
    except Exception:
        return None, None
    exp = payload.get("exp")
    if not exp:
        return None, None
    expire_dt = datetime.datetime.fromtimestamp(exp)
    remaining = exp - time.time()
    return remaining / 86400, expire_dt.strftime("%Y-%m-%d %H:%M:%S")



# --- 用户记忆（fact 列表 + LRU 淘汰）---

_MEMORY_MAX_ENTRIES = 100


def _get_user_memory_path(staff_id: str) -> Path:
    personal_dir = os.environ.get("TYCLAW_PERSONAL_DIR", "")
    if personal_dir:
        return Path(personal_dir) / "memory" / "memory.yaml"
    return workspace_path(get_works_dir(), staff_id) / "memory" / "memory.yaml"


def load_user_memory(staff_id: str) -> list[dict]:
    """读取用户记忆列表，按 last_used 倒序"""
    mem_path = _get_user_memory_path(staff_id)
    if not mem_path.exists():
        return []
    try:
        with open(mem_path, "r", encoding="utf-8") as f:
            entries = yaml.safe_load(f)
        if not isinstance(entries, list):
            return []
        return entries
    except (yaml.YAMLError, OSError):
        return []


def save_user_memory(staff_id: str, entries: list[dict]):
    """保存记忆列表（调用前应已排序）"""
    mem_path = _get_user_memory_path(staff_id)
    mem_path.parent.mkdir(parents=True, exist_ok=True)
    with open(mem_path, "w", encoding="utf-8") as f:
        yaml.dump(entries, f, allow_unicode=True, default_flow_style=False)


def _get_installed_skills_path(staff_id: str) -> Path:
    personal_dir = os.environ.get("TYCLAW_PERSONAL_DIR", "")
    if personal_dir:
        return Path(personal_dir) / "installed_skills.json"
    return workspace_path(get_works_dir(), staff_id) / "installed_skills.json"


def load_installed_skills(staff_id: str) -> list[str]:
    """读取用户已安装的 optional builtin skill key 列表"""
    path = _get_installed_skills_path(staff_id)
    if not path.exists():
        return []
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
        return data if isinstance(data, list) else []
    except (json.JSONDecodeError, OSError):
        return []


def save_installed_skills(staff_id: str, keys: list[str]):
    """保存用户已安装的 optional builtin skill key 列表"""
    path = _get_installed_skills_path(staff_id)
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(".tmp")
    tmp.write_text(json.dumps(keys, ensure_ascii=False), encoding="utf-8")
    tmp.rename(path)


def _next_memory_id(entries: list[dict]) -> str:
    max_n = 0
    for e in entries:
        mid = e.get("id", "")
        if mid.startswith("m") and mid[1:].isdigit():
            max_n = max(max_n, int(mid[1:]))
    return f"m{max_n + 1}"


def _now_iso() -> str:
    from datetime import datetime as _dt
    return _dt.now().strftime("%Y-%m-%dT%H:%M:%S")


def apply_memory_ops(staff_id: str, ops: list[tuple[str, str]]) -> int:
    """执行记忆操作列表，返回实际变更数。

    ops 格式: [("引用", "m1, m2"), ("新增", "text"), ("替换", "m2 -> text"), ("删除", "m3")]
    """
    import re as _re
    entries = load_user_memory(staff_id)
    changed = 0
    now = _now_iso()
    id_index = {e["id"]: e for e in entries}

    for op_type, op_value in ops:
        value = op_value.strip()
        if not value:
            continue

        if op_type == "引用":
            for mid in _re.split(r"[,，\s]+", value):
                mid = mid.strip()
                if mid in id_index:
                    id_index[mid]["last_used"] = now
                    changed += 1

        elif op_type == "新增":
            new_id = _next_memory_id(entries)
            new_entry = {"id": new_id, "text": value, "last_used": now}
            entries.append(new_entry)
            id_index[new_id] = new_entry
            changed += 1
            if len(entries) > _MEMORY_MAX_ENTRIES:
                entries.sort(key=lambda e: e.get("last_used", ""), reverse=True)
                evicted = entries.pop()
                del id_index[evicted["id"]]

        elif op_type == "替换":
            parts = value.split("->", 1)
            if len(parts) != 2:
                continue
            old_id = parts[0].strip()
            new_text = parts[1].strip()
            if not new_text:
                continue
            if old_id in id_index:
                del id_index[old_id]
                entries = [e for e in entries if e["id"] != old_id]
            new_id = _next_memory_id(entries)
            new_entry = {"id": new_id, "text": new_text, "last_used": now}
            entries.append(new_entry)
            id_index[new_id] = new_entry
            changed += 1

        elif op_type == "删除":
            mid = value.strip()
            if mid in id_index:
                del id_index[mid]
                entries = [e for e in entries if e["id"] != mid]
                changed += 1

    if changed:
        entries.sort(key=lambda e: e.get("last_used", ""), reverse=True)
        save_user_memory(staff_id, entries)

    return changed


def print_output(data, fmt="json"):
    """统一输出函数"""
    if fmt == "json":
        print(format_json(data))
    elif fmt == "markdown":
        if isinstance(data, dict) and "headers" in data and "rows" in data:
            print(format_markdown_table(data["headers"], data["rows"]))
        else:
            print(format_json(data))
    else:
        print(format_json(data))
