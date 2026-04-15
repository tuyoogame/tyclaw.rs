"""
GA 平台 SQL 查询工具
通过 MySQL 协议连接 GA 数据网关执行 SQL，查询事件/用户/设备/分群等数据

用法:
  python tools/ga_query.py --sql "SELECT ..." --format markdown
  python tools/ga_query.py --list-tables
  python tools/ga_query.py --discover
  python tools/ga_query.py --sql "SELECT ..." --project-id 20249
"""

import argparse
import os
import sys

import pymysql

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
from utils import (load_user_config, load_user_credentials, format_json, format_markdown_table,
                   get_injected_credential, save_user_credentials, clear_user_credentials,
                   sync_credential_env, clear_credential_env)
from defaults import GA_CONFIG


_NO_CRED_MSG = "你还没有设置 GA 账号，请发送「设置GA凭证」进行配置。"
_BAD_CRED_MSG = "GA 认证失败（账号或密码错误），请发送「设置GA凭证」重新配置。"


def _check_ga_credentials(staff_id: str) -> str | None:
    """校验 GA 凭证是否已配置，返回错误信息或 None。
    优先检查 Bot 注入的环境变量，回退到 _personal/ 文件。"""
    if (get_injected_credential("ga", "username")
            and get_injected_credential("ga", "password")):
        return None
    if not staff_id:
        return _NO_CRED_MSG
    ga = load_user_credentials(staff_id).get("ga")
    if not ga or not ga.get("username") or not ga.get("password"):
        return _NO_CRED_MSG
    return None


def _get_connection(config):
    """创建 MySQL 连接，host/port/ssl 取全局常量，user/password 优先取注入凭证"""
    username = get_injected_credential("ga", "username")
    password = get_injected_credential("ga", "password")
    if not username or not password:
        ga = config["ga"]
        username, password = ga["username"], ga["password"]
    ssl_arg = {"ssl": {}} if GA_CONFIG.get("ssl", True) else None
    return pymysql.connect(
        host=GA_CONFIG["host"],
        port=GA_CONFIG["port"],
        user=username,
        password=password,
        ssl=ssl_arg,
        connect_timeout=10,
        read_timeout=330,
    )


def execute_sql(config, sql, staff_id: str = ""):
    """执行 GA SQL 查询（MySQL 协议），返回格式与原 REST API 兼容"""
    err = _check_ga_credentials(staff_id)
    if err:
        return {"ifSuccess": False, "error": err}
    try:
        conn = _get_connection(config)
        with conn:
            with conn.cursor() as cur:
                cur.execute(sql)
                if cur.description is None:
                    return {"ifSuccess": True, "header": [], "result": []}
                header = [desc[0] for desc in cur.description]
                rows = [dict(zip(header, row)) for row in cur.fetchall()]
                return {"ifSuccess": True, "header": header, "result": rows}
    except pymysql.err.OperationalError as e:
        code = e.args[0] if e.args else None
        if code == 1045:
            return {"ifSuccess": False, "error": _BAD_CRED_MSG}
        return {"ifSuccess": False, "error": str(e)}
    except Exception as e:
        return {"ifSuccess": False, "error": str(e)}


def list_projects(config, staff_id: str = ""):
    """通过 SHOW PROJECTS 获取当前用户可访问的所有项目 ID"""
    err = _check_ga_credentials(staff_id)
    if err:
        return {"error": err}
    try:
        conn = _get_connection(config)
        with conn:
            with conn.cursor() as cur:
                rows = _fetch_rows(cur, "SHOW PROJECTS")
                return [{"project_id": row.get("Database", "")} for row in rows]
    except pymysql.err.OperationalError as e:
        code = e.args[0] if e.args else None
        if code == 1045:
            return {"error": _BAD_CRED_MSG}
        return {"error": str(e)}
    except Exception as e:
        return {"error": str(e)}


def list_tables(config, project_id: str, staff_id: str = ""):
    """通过 show tables in <pid> 动态获取项目可用表"""
    err = _check_ga_credentials(staff_id)
    if err:
        return {"error": err}
    pid = str(project_id)
    try:
        conn = _get_connection(config)
        with conn:
            with conn.cursor() as cur:
                cur.execute(f"show tables in {pid}")
                col = cur.description[0][0] if cur.description else "table"
                return [{"table_name": row[0]} for row in cur.fetchall()]
    except pymysql.err.OperationalError as e:
        code = e.args[0] if e.args else None
        if code == 1045:
            return {"error": _BAD_CRED_MSG}
        return {"error": str(e)}
    except Exception as e:
        return {"error": str(e)}


def _fetch_rows(cur, sql):
    """执行 SQL 并返回 list[dict]，无结果返回空列表"""
    cur.execute(sql)
    if not cur.description:
        return []
    header = [desc[0] for desc in cur.description]
    return [dict(zip(header, row)) for row in cur.fetchall()]


def discover_tables(config, project_id: str, staff_id: str = ""):
    """通过元数据命令探查项目 schema：表列表、事件属性、维度属性"""
    err = _check_ga_credentials(staff_id)
    if err:
        return {"error": err}
    pid = str(project_id)
    try:
        conn = _get_connection(config)
        with conn:
            with conn.cursor() as cur:
                tables = _fetch_rows(cur, f"SHOW TABLES IN {pid}")
                dims = _fetch_rows(cur, f"SHOW DIMS IN {pid}")
                event_props = _fetch_rows(cur, f"SHOW EVENT_PROPERTIES IN {pid}")
                dim_props = {}
                for d in dims:
                    alias = d.get("alias", "")
                    if alias:
                        props = _fetch_rows(
                            cur,
                            f"SHOW PROPERTIES IN {pid} WHERE dimension = '{alias}'",
                        )
                        dim_props[alias] = props
        return {
            "tables": tables,
            "dims": dims,
            "event_properties": event_props,
            "dim_properties": dim_props,
        }
    except pymysql.err.OperationalError as e:
        code = e.args[0] if e.args else None
        if code == 1045:
            return {"error": _BAD_CRED_MSG}
        return {"error": str(e)}
    except Exception as e:
        return {"error": str(e)}


def _cmd_setup(args):
    staff_id = os.environ.get("TYCLAW_SENDER_STAFF_ID", "")
    if not staff_id:
        print("Error: TYCLAW_SENDER_STAFF_ID env var required", file=sys.stderr)
        sys.exit(1)
    data = {"username": args.username, "password": args.password}
    save_user_credentials(staff_id, "ga", data)
    sync_credential_env("ga", data)
    print(f"GA credentials saved for {staff_id}")


def _cmd_clear_credentials(_args):
    staff_id = os.environ.get("TYCLAW_SENDER_STAFF_ID", "")
    if not staff_id:
        print("Error: TYCLAW_SENDER_STAFF_ID env var required", file=sys.stderr)
        sys.exit(1)
    if clear_user_credentials(staff_id, "ga"):
        clear_credential_env("ga")
        print(f"GA credentials cleared for {staff_id}")
    else:
        print(f"No GA credentials found for {staff_id}")


def main():
    parser = argparse.ArgumentParser(description="GA SQL query tool")
    sub = parser.add_subparsers(dest="_sub")

    p_setup = sub.add_parser("setup", help="Set GA credentials")
    p_setup.add_argument("--username", required=True)
    p_setup.add_argument("--password", required=True)
    sub.add_parser("clear-credentials", help="Clear GA credentials")

    parser.add_argument("--config", help="Path to config.yaml")
    parser.add_argument("--sql", help="SQL query to execute")
    parser.add_argument("--list-projects", action="store_true",
                        help="List accessible project IDs")
    parser.add_argument("--list-tables", action="store_true",
                        help="List available GA tables")
    parser.add_argument("--discover", action="store_true",
                        help="Discover project schema via metadata commands")
    parser.add_argument("--project-id", help="Override GA project ID")
    parser.add_argument("--format", choices=["json", "markdown"],
                        default="json", help="Output format")
    parser.add_argument("--max-length", type=int, default=0,
                        help="Max output characters (0 = no limit)")

    args = parser.parse_args()

    if args._sub == "setup":
        _cmd_setup(args)
        return
    if args._sub == "clear-credentials":
        _cmd_clear_credentials(args)
        return

    staff_id = os.environ.get("TYCLAW_SENDER_STAFF_ID", "")
    config = load_user_config(staff_id, args.config)

    err = _check_ga_credentials(staff_id)
    if err:
        print(format_json({"ifSuccess": False, "error": err}))
        sys.exit(1)

    if args.list_projects:
        print("Listing accessible projects...", file=sys.stderr)
        result = list_projects(config, staff_id)
        print(format_json(result))
        return

    if args.list_tables:
        if not args.project_id:
            parser.error("--project-id is required for --list-tables")
        print("Listing GA tables...", file=sys.stderr)
        result = list_tables(config, args.project_id, staff_id)
        print(format_json(result))
        return

    if args.discover:
        if not args.project_id:
            parser.error("--project-id is required for --discover")
        print("Discovering GA table fields...", file=sys.stderr)
        result = discover_tables(config, args.project_id, staff_id)
        print(format_json(result))
        return

    if not args.sql:
        parser.error("--sql is required for querying. "
                      "Use --list-tables or --discover to explore.")

    print("Executing GA SQL query...", file=sys.stderr)
    result = execute_sql(config, args.sql, staff_id)

    if not result.get("ifSuccess", False):
        error_msg = result.get("error", "Unknown error")
        print(f"Query failed: {error_msg}", file=sys.stderr)
        print(format_json(result))
        return

    if (args.format == "markdown"
            and "header" in result and "result" in result):
        headers = result["header"]
        rows = result["result"]
        print(f"Query returned {len(rows)} rows", file=sys.stderr)
        output = format_markdown_table(headers, rows)
        if args.max_length > 0:
            output = output[:args.max_length] + "\n...(truncated)"
        print(output)
        return

    output = format_json(result)
    if args.max_length > 0:
        output = output[:args.max_length] + "\n...(truncated)"
    print(output)


if __name__ == "__main__":
    main()
