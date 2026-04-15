"""
企业邮箱读取工具（只读）
通过阿里企业邮箱 IMAP 读取和搜索邮件，不执行任何写操作

用法:
  python tools/email_reader.py list
  python tools/email_reader.py list --limit 20 --folder "Sent Messages"
  python tools/email_reader.py read --id 123
  python tools/email_reader.py search --subject "周报"
  python tools/email_reader.py search --from "zhang@tuyoogame.com" --since 2025-01-01
"""

import argparse
import email
import email.header
import email.message
import email.utils
import html
import imaplib
import os
import re
import ssl
import sys
from datetime import datetime

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
from utils import load_user_config, get_injected_credential

IMAP_HOST = "imap.qiye.aliyun.com"
IMAP_PORT = 993
DEFAULT_LIMIT = 10
MAX_BODY_CHARS = 8000


def _get_email_config() -> tuple[str, str]:
    """读取邮箱凭证，与 email_sender.py 逻辑一致"""
    address = get_injected_credential("email", "address")
    password = get_injected_credential("email", "password")
    if not address or not password:
        staff_id = os.environ.get("TYCLAW_SENDER_STAFF_ID", "")
        config = load_user_config(staff_id)
        email_cfg = config.get("email", {})
        address = email_cfg.get("address", "")
        password = email_cfg.get("password", "")
    if not address or not password:
        print("Error: email credentials not configured. "
              "Please set up with: 设置我的邮箱", file=sys.stderr)
        sys.exit(1)
    return address, password


def _connect(folder: str = "INBOX") -> imaplib.IMAP4_SSL:
    address, password = _get_email_config()
    context = ssl.create_default_context()
    client = imaplib.IMAP4_SSL(IMAP_HOST, IMAP_PORT, ssl_context=context)
    client.login(address, password)
    client.select(folder, readonly=True)
    return client


def _decode_header(raw: str | None) -> str:
    if not raw:
        return ""
    parts = email.header.decode_header(raw)
    decoded = []
    for data, charset in parts:
        if isinstance(data, bytes):
            decoded.append(data.decode(charset or "utf-8", errors="replace"))
        else:
            decoded.append(data)
    return " ".join(decoded)


def _parse_date(msg: email.message.Message) -> str:
    raw = msg.get("Date", "")
    parsed = email.utils.parsedate_to_datetime(raw) if raw else None
    if parsed:
        return parsed.strftime("%Y-%m-%d %H:%M")
    return raw


def _extract_text(msg: email.message.Message) -> str:
    """提取邮件正文，纯文本优先，HTML fallback 去标签"""
    text_parts: list[str] = []
    html_parts: list[str] = []

    if msg.is_multipart():
        for part in msg.walk():
            ct = part.get_content_type()
            if part.get("Content-Disposition", "").startswith("attachment"):
                continue
            payload = part.get_payload(decode=True)
            if not payload:
                continue
            charset = part.get_content_charset() or "utf-8"
            text = payload.decode(charset, errors="replace")
            if ct == "text/plain":
                text_parts.append(text)
            elif ct == "text/html":
                html_parts.append(text)
    else:
        payload = msg.get_payload(decode=True)
        if payload:
            charset = msg.get_content_charset() or "utf-8"
            text = payload.decode(charset, errors="replace")
            if msg.get_content_type() == "text/html":
                html_parts.append(text)
            else:
                text_parts.append(text)

    if text_parts:
        body = "\n".join(text_parts)
    elif html_parts:
        body = _html_to_text("\n".join(html_parts))
    else:
        body = "(no text content)"

    if len(body) > MAX_BODY_CHARS:
        body = body[:MAX_BODY_CHARS] + f"\n... (truncated, total {len(body)} chars)"
    return body


def _html_to_text(h: str) -> str:
    """简易 HTML 转文本：去标签、解码实体"""
    h = re.sub(r"<br\s*/?>", "\n", h, flags=re.IGNORECASE)
    h = re.sub(r"</(p|div|tr|li|h[1-6])>", "\n", h, flags=re.IGNORECASE)
    h = re.sub(r"<[^>]+>", "", h)
    h = html.unescape(h)
    lines = [line.strip() for line in h.splitlines()]
    return "\n".join(line for line in lines if line)


def _list_attachments(msg: email.message.Message) -> list[str]:
    names = []
    if not msg.is_multipart():
        return names
    for part in msg.walk():
        disp = part.get("Content-Disposition", "")
        if "attachment" in disp:
            fn = _decode_header(part.get_filename())
            if fn:
                names.append(fn)
    return names


def _fmt_summary(seq: bytes, msg: email.message.Message) -> str:
    mid = seq.decode()
    frm = _decode_header(msg.get("From"))
    subj = _decode_header(msg.get("Subject"))
    date = _parse_date(msg)
    return f"[{mid}] {date}  From: {frm}\n      Subject: {subj}"


def cmd_list(client: imaplib.IMAP4_SSL, limit: int):
    status, data = client.search(None, "ALL")
    if status != "OK" or not data[0]:
        print("Inbox is empty")
        return
    ids = data[0].split()
    selected = ids[-limit:] if len(ids) > limit else ids
    # 从新到旧
    selected.reverse()

    print(f"Inbox: {len(ids)} total, showing latest {len(selected)}\n")
    for seq in selected:
        status, msg_data = client.fetch(seq, "(RFC822.HEADER)")
        if status != "OK" or not msg_data or not msg_data[0]:
            continue
        raw = msg_data[0][1] if isinstance(msg_data[0], tuple) else msg_data[0]
        msg = email.message_from_bytes(raw)
        print(_fmt_summary(seq, msg))


def cmd_read(client: imaplib.IMAP4_SSL, msg_id: str):
    status, msg_data = client.fetch(msg_id.encode(), "(RFC822)")
    if status != "OK" or not msg_data or not msg_data[0]:
        print(f"Error: message {msg_id} not found", file=sys.stderr)
        sys.exit(1)

    raw = msg_data[0][1] if isinstance(msg_data[0], tuple) else msg_data[0]
    msg = email.message_from_bytes(raw)

    print(f"From:    {_decode_header(msg.get('From'))}")
    print(f"To:      {_decode_header(msg.get('To'))}")
    cc = _decode_header(msg.get("Cc"))
    if cc:
        print(f"Cc:      {cc}")
    print(f"Date:    {_parse_date(msg)}")
    print(f"Subject: {_decode_header(msg.get('Subject'))}")

    attachments = _list_attachments(msg)
    if attachments:
        print(f"Attachments: {', '.join(attachments)}")

    print(f"\n{'='*60}\n")
    print(_extract_text(msg))


def cmd_search(client: imaplib.IMAP4_SSL, *,
               from_addr: str, subject: str,
               since: str, before: str,
               limit: int):
    if not any([from_addr, subject, since, before]):
        print("Error: at least one search criterion required "
              "(--from, --subject, --since, --before)", file=sys.stderr)
        sys.exit(1)

    # 阿里企业邮箱 IMAP 仅支持日期类 SEARCH，FROM/SUBJECT 等内容搜索不可用
    # 策略：日期条件走服务端缩小范围，内容条件在客户端过滤 headers
    server_criteria = "ALL"
    date_parts: list[str] = []
    if since:
        date_parts.append(f"SINCE {_fmt_imap_date(since)}")
    if before:
        date_parts.append(f"BEFORE {_fmt_imap_date(before)}")
    if date_parts:
        server_criteria = " ".join(date_parts)

    status, data = client.search(None, server_criteria)
    if status != "OK" or not data[0]:
        print("No messages found")
        return

    ids = data[0].split()

    from_lower = from_addr.lower() if from_addr else ""
    subject_lower = subject.lower() if subject else ""
    matched: list[tuple[bytes, email.message.Message]] = []

    # 从新到旧遍历，客户端过滤
    for seq in reversed(ids):
        if len(matched) >= limit:
            break
        status, msg_data = client.fetch(seq, "(RFC822.HEADER)")
        if status != "OK" or not msg_data or not msg_data[0]:
            continue
        raw = msg_data[0][1] if isinstance(msg_data[0], tuple) else msg_data[0]
        msg = email.message_from_bytes(raw)
        if from_lower and from_lower not in _decode_header(msg.get("From")).lower():
            continue
        if subject_lower and subject_lower not in _decode_header(msg.get("Subject")).lower():
            continue
        matched.append((seq, msg))

    if not matched:
        desc_parts = []
        if from_addr:
            desc_parts.append(f"from={from_addr}")
        if subject:
            desc_parts.append(f"subject={subject}")
        if since:
            desc_parts.append(f"since={since}")
        if before:
            desc_parts.append(f"before={before}")
        print(f"No messages found matching: {', '.join(desc_parts)}")
        return

    total_label = f"(scanned {len(ids)})" if len(ids) != len(matched) else ""
    print(f"Found {len(matched)} message(s) {total_label}\n")
    for seq, msg in matched:
        print(_fmt_summary(seq, msg))


def _fmt_imap_date(date_str: str) -> str:
    """将 YYYY-MM-DD 转为 IMAP 日期格式 DD-Mon-YYYY"""
    dt = datetime.strptime(date_str, "%Y-%m-%d")
    return dt.strftime("%d-%b-%Y")


def main():
    parser = argparse.ArgumentParser(
        description="Read email via enterprise mailbox (readonly)")
    sub = parser.add_subparsers(dest="command", required=True)

    p_list = sub.add_parser("list", help="List recent messages")
    p_list.add_argument("--limit", type=int, default=DEFAULT_LIMIT,
                        help=f"Number of messages to show (default {DEFAULT_LIMIT})")
    p_list.add_argument("--folder", default="INBOX",
                        help="Mailbox folder (default INBOX)")

    p_read = sub.add_parser("read", help="Read a specific message")
    p_read.add_argument("--id", required=True, dest="msg_id",
                        help="Message sequence number (from list output)")
    p_read.add_argument("--folder", default="INBOX",
                        help="Mailbox folder (default INBOX)")

    p_search = sub.add_parser("search", help="Search messages")
    p_search.add_argument("--from", default="", dest="from_addr",
                          help="Sender address (partial match)")
    p_search.add_argument("--subject", default="",
                          help="Subject keyword")
    p_search.add_argument("--since", default="",
                          help="Messages since date (YYYY-MM-DD)")
    p_search.add_argument("--before", default="",
                          help="Messages before date (YYYY-MM-DD)")
    p_search.add_argument("--limit", type=int, default=DEFAULT_LIMIT,
                          help=f"Max results (default {DEFAULT_LIMIT})")
    p_search.add_argument("--folder", default="INBOX",
                          help="Mailbox folder (default INBOX)")

    args = parser.parse_args()

    client = _connect(args.folder)
    try:
        if args.command == "list":
            cmd_list(client, args.limit)
        elif args.command == "read":
            cmd_read(client, args.msg_id)
        elif args.command == "search":
            cmd_search(client, from_addr=args.from_addr,
                       subject=args.subject, since=args.since,
                       before=args.before, limit=args.limit)
    finally:
        try:
            client.logout()
        except Exception:
            pass


if __name__ == "__main__":
    main()
