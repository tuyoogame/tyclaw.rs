"""
定时任务 JSON 持久化存储（纯标准库 + hashlib，无 bot/ 依赖）
支持两种模式：
  - 目录模式（bot 进程）：扫描 works_dir/*/*/schedules.json（分桶），mtime 缓存
  - 文件模式（容器内 tool）：单用户单文件
两种模式均使用 fcntl.flock 保证跨进程写入安全。
"""

import fcntl
import hashlib
import json
import logging
import os
import threading
import uuid
from datetime import datetime
from pathlib import Path

logger = logging.getLogger("tyclaw")

MAX_SCHEDULES_PER_USER = 20
_USER_QUOTA_OVERRIDES: dict[str, int] = {
    "03265124623377": 50,
}
_SCHEDULES_FILENAME = "schedules.json"


class ScheduleStore:
    """定时任务持久化，per-user 文件隔离，mtime 缓存 + fcntl.flock

    目录模式（bot 进程）:
        ScheduleStore("/path/to/works")
        扫描 works/*/*/schedules.json（分桶），mtime 缓存跳过未变更文件

    文件模式（容器内 tool）:
        ScheduleStore("/path/to/schedules.json", staff_id="xxx")
        直接读写单个用户的文件
    """

    def __init__(self, path: str, *, staff_id: str = ""):
        """
        Args:
            path: 目录模式传 works/ 目录路径；文件模式传 schedules.json 文件路径
            staff_id: 非空时启用文件模式（容器内单用户）
        """
        self._lock = threading.Lock()
        self._data: dict[str, list[dict]] = {}
        self._mtime_cache: dict[str, float] = {}

        if staff_id:
            self._mode = "file"
            self._base_dir: Path | None = None
            self._file_path = Path(path)
            self._staff_id = staff_id
            self._load_user_file(staff_id, self._file_path)
        else:
            self._mode = "dir"
            self._base_dir = Path(path)
            self._file_path = None
            self._staff_id = ""
            self._scan_all()

    @property
    def _path(self) -> Path:
        """向后兼容：日志中引用 store._path"""
        return self._file_path if self._mode == "file" else self._base_dir

    # ── File I/O with flock ──

    @staticmethod
    def _read_file(path: Path) -> list[dict]:
        try:
            with open(path, "r", encoding="utf-8") as f:
                fcntl.flock(f, fcntl.LOCK_SH)
                data = json.load(f)
            if isinstance(data, list):
                return data
            # 兼容旧格式 {staff_id: [...]}
            if isinstance(data, dict):
                for v in data.values():
                    if isinstance(v, list):
                        return v
            return []
        except FileNotFoundError:
            return []
        except (json.JSONDecodeError, OSError):
            logger.exception("Failed to read schedules from %s", path)
            return []

    @staticmethod
    def _write_file(path: Path, schedules: list[dict]):
        path.parent.mkdir(parents=True, exist_ok=True)
        try:
            with open(path, "w", encoding="utf-8") as f:
                fcntl.flock(f, fcntl.LOCK_EX)
                f.write(json.dumps(schedules, ensure_ascii=False, indent=2))
                f.flush()
                os.fsync(f.fileno())
        except Exception:
            logger.exception("Failed to save schedules to %s", path)

    # ── Internal helpers ──

    def _scan_all(self, quiet: bool = False):
        """目录模式：glob + mtime 缓存，跳过未变更文件（两级分桶目录）"""
        found: set[str] = set()
        for f in self._base_dir.glob(f"*/*/{_SCHEDULES_FILENAME}"):
            sid = f.parent.name
            if sid.startswith("."):
                continue
            found.add(sid)
            try:
                mt = f.stat().st_mtime
            except OSError:
                continue
            if self._mtime_cache.get(sid) == mt:
                continue
            self._data[sid] = self._read_file(f)
            self._mtime_cache[sid] = mt

        for sid in list(self._data):
            if sid not in found:
                self._data.pop(sid, None)
                self._mtime_cache.pop(sid, None)

        if not quiet:
            total = sum(len(v) for v in self._data.values())
            logger.info("Loaded schedules: %d user(s), %d task(s)",
                        len(self._data), total)

    def _load_user_file(self, staff_id: str, path: Path):
        self._data[staff_id] = self._read_file(path)

    def _user_file(self, staff_id: str) -> Path:
        if self._mode == "file":
            return self._file_path
        bucket = f"{hashlib.md5(staff_id.encode()).digest()[0]:02x}"
        return self._base_dir / bucket / staff_id / _SCHEDULES_FILENAME

    def _save_user(self, staff_id: str):
        """保存单用户数据，调用方需已持有 self._lock"""
        schedules = self._data.get(staff_id, [])
        path = self._user_file(staff_id)
        self._write_file(path, schedules)
        try:
            self._mtime_cache[staff_id] = path.stat().st_mtime
        except OSError:
            pass

    # ── Public API ──

    def reload(self):
        """热加载（目录模式用 mtime 缓存，文件模式重读）"""
        with self._lock:
            if self._mode == "dir":
                self._scan_all(quiet=True)
            else:
                self._load_user_file(self._staff_id, self._file_path)

    def get(self, staff_id: str, schedule_id: str) -> dict | None:
        with self._lock:
            for s in self._data.get(staff_id, []):
                if s["id"] == schedule_id:
                    return dict(s)
            return None

    def get_user_schedules(self, staff_id: str) -> list[dict]:
        with self._lock:
            return list(self._data.get(staff_id, []))

    def add(self, staff_id: str, name: str, cron: str, message: str,
            conversation_type: str = "1",
            conversation_id: str = "",
            conversation_title: str = "",
            end_at: str = "") -> dict | None:
        """添加定时任务，返回新建的 schedule dict；超限返回 None

        Args:
            end_at: 可选，ISO 格式截止时间（如 "2025-04-11T10:00:00"），到期自动停用
        """
        with self._lock:
            user_schedules = self._data.setdefault(staff_id, [])
            quota = _USER_QUOTA_OVERRIDES.get(staff_id, MAX_SCHEDULES_PER_USER)
            if len(user_schedules) >= quota:
                return None
            sched = {
                "id": uuid.uuid4().hex[:8],
                "name": name,
                "cron": cron,
                "message": message,
                "enabled": True,
                "created_at": datetime.now().isoformat(
                    timespec="seconds"),
                "last_run": None,
                "conversation_type": conversation_type,
                "conversation_id": conversation_id,
                "conversation_title": conversation_title,
            }
            if end_at:
                sched["end_at"] = end_at
            user_schedules.append(sched)
            self._save_user(staff_id)
            return sched

    def remove(self, staff_id: str, schedule_id: str) -> bool:
        with self._lock:
            schedules = self._data.get(staff_id, [])
            for i, s in enumerate(schedules):
                if s["id"] == schedule_id:
                    schedules.pop(i)
                    if not schedules:
                        self._data.pop(staff_id, None)
                    self._save_user(staff_id)
                    return True
            return False

    def toggle(self, staff_id: str, schedule_id: str) -> dict | None:
        """切换启用/禁用，返回更新后的 schedule 或 None"""
        with self._lock:
            for s in self._data.get(staff_id, []):
                if s["id"] == schedule_id:
                    s["enabled"] = not s["enabled"]
                    self._save_user(staff_id)
                    return s
            return None

    def update(self, staff_id: str, schedule_id: str, *,
               name: str | None = None,
               cron: str | None = None,
               message: str | None = None,
               end_at: str | None = None) -> dict | None:
        """更新指定字段，保留未传入的字段不变。返回更新后的 schedule 或 None

        Args:
            end_at: 传空字符串可清除截止时间
        """
        with self._lock:
            for s in self._data.get(staff_id, []):
                if s["id"] == schedule_id:
                    if name is not None:
                        s["name"] = name
                    if cron is not None:
                        s["cron"] = cron
                    if message is not None:
                        s["message"] = message
                    if end_at is not None:
                        if end_at:
                            s["end_at"] = end_at
                        else:
                            s.pop("end_at", None)
                    self._save_user(staff_id)
                    return dict(s)
            return None

    def mark_run(self, staff_id: str, schedule_id: str):
        with self._lock:
            for s in self._data.get(staff_id, []):
                if s["id"] == schedule_id:
                    s["last_run"] = datetime.now().isoformat(
                        timespec="seconds")
                    self._save_user(staff_id)
                    return

    def disable(self, staff_id: str, schedule_id: str) -> bool:
        """禁用指定定时任务，返回是否找到"""
        with self._lock:
            for s in self._data.get(staff_id, []):
                if s["id"] == schedule_id:
                    s["enabled"] = False
                    self._save_user(staff_id)
                    return True
            return False

    def update_log(self, staff_id: str, schedule_id: str,
                   log_filename: str) -> None:
        """记录最近一次执行的日志文件名"""
        with self._lock:
            for s in self._data.get(staff_id, []):
                if s["id"] == schedule_id:
                    s["last_log_filename"] = log_filename
                    self._save_user(staff_id)
                    return

    def all_schedules(self) -> list[tuple[str, dict]]:
        """返回所有 (staff_id, schedule) 对"""
        with self._lock:
            result = []
            for staff_id, schedules in self._data.items():
                for s in schedules:
                    result.append((staff_id, dict(s)))
            return result

    def all_enabled(self) -> list[tuple[str, dict]]:
        """返回所有启用的 (staff_id, schedule) 对"""
        with self._lock:
            result = []
            for staff_id, schedules in self._data.items():
                for s in schedules:
                    if s.get("enabled", True):
                        result.append((staff_id, dict(s)))
            return result
