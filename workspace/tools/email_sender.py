"""
企业邮箱发送工具
通过阿里企业邮箱 SMTP 发送邮件，支持 HTML 正文、附件和内嵌图片

用法:
  python tools/email_sender.py --to "a@tuyoogame.com" --subject "标题" --body "正文"
  python tools/email_sender.py --to "a@x.com,b@x.com" --subject "标题" --body "<h1>Hi</h1>" --html
  python tools/email_sender.py --to "a@x.com" --cc "b@x.com" --subject "标题" --body "正文" --attachment "/tmp/report.xlsx"
  python tools/email_sender.py --to "a@x.com" --subject "日报" --inline-image "/tmp/a.png,/tmp/b.png"
"""

import argparse
import mimetypes
import os
import smtplib
import ssl
import sys
from email.header import Header
from email.mime.base import MIMEBase
from email.mime.image import MIMEImage
from email.mime.multipart import MIMEMultipart
from email.mime.text import MIMEText
from email.utils import formataddr, formatdate, make_msgid
from email import encoders
from pathlib import Path

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
from utils import (load_user_config, get_injected_credential, save_user_credentials,
                   clear_user_credentials, sync_credential_env, clear_credential_env)

SMTP_HOST = "smtp.qiye.aliyun.com"
SMTP_PORT = 465


def _get_email_config() -> tuple[str, str]:
    """读取当前用户的邮箱凭证，返回 (address, password)。优先使用 Bot 注入的环境变量。"""
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


def _attach_file(msg: MIMEMultipart, filepath: str):
    """将文件作为附件添加到邮件"""
    p = Path(filepath)
    if not p.exists():
        print(f"Warning: attachment not found, skipped: {filepath}",
              file=sys.stderr)
        return
    ctype, _ = mimetypes.guess_type(str(p))
    if ctype is None:
        ctype = "application/octet-stream"
    maintype, subtype = ctype.split("/", 1)
    with open(p, "rb") as f:
        part = MIMEBase(maintype, subtype)
        part.set_payload(f.read())
    encoders.encode_base64(part)
    part.add_header("Content-Disposition", "attachment",
                    filename=("utf-8", "", p.name))
    msg.attach(part)


ALLOWED_DOMAIN = "tuyoogame.com"


def _validate_recipients(recipients: list[str]):
    """校验所有收件人必须是公司邮箱，防止信息外泄"""
    blocked = [r for r in recipients
               if not r.lower().endswith(f"@{ALLOWED_DOMAIN}")]
    if blocked:
        print(f"Error: recipients outside @{ALLOWED_DOMAIN} "
              f"are not allowed: {', '.join(blocked)}",
              file=sys.stderr)
        sys.exit(1)


def send_email(*, to: list[str], subject: str, body: str,
               cc: list[str] | None = None,
               attachments: list[str] | None = None,
               inline_images: list[str] | None = None,
               html: bool = False):
    sender_addr, password = _get_email_config()
    cc = cc or []
    attachments = attachments or []
    inline_images = inline_images or []

    _validate_recipients(to + cc)

    msg = MIMEMultipart("mixed")
    msg["From"] = formataddr(("TyClaw", sender_addr))
    msg["To"] = ", ".join(to)
    if cc:
        msg["Cc"] = ", ".join(cc)
    msg["Subject"] = Header(subject, "utf-8")
    msg["Date"] = formatdate(localtime=True)
    msg["Message-ID"] = make_msgid()

    # 有内嵌图片时，用 related 子结构包裹 HTML 和图片
    if inline_images:
        related = MIMEMultipart("related")
        img_html_parts = []
        cid_map: list[tuple[str, Path]] = []
        for i, fp in enumerate(inline_images):
            p = Path(fp)
            if not p.exists():
                print(f"Warning: inline image not found, skipped: {fp}",
                      file=sys.stderr)
                continue
            cid = f"img{i}"
            cid_map.append((cid, p))
            # 用文件名（去掉扩展名和序号前缀）作为图片标题
            title = p.stem.lstrip("0123456789_")
            img_html_parts.append(
                f'<div style="margin-bottom:16px">'
                f'<div style="font-weight:bold;margin-bottom:4px">{title}</div>'
                f'<img src="cid:{cid}" style="max-width:100%" />'
                f'</div>'
            )
        # 拼接最终 HTML：用户 body + 内嵌图片
        if body:
            full_html = body if html else f"<p>{body}</p>"
            full_html += "\n" + "\n".join(img_html_parts)
        else:
            full_html = "\n".join(img_html_parts)
        related.attach(MIMEText(full_html, "html", "utf-8"))
        for cid, p in cid_map:
            ctype, _ = mimetypes.guess_type(str(p))
            subtype = ctype.split("/", 1)[1] if ctype and "/" in ctype else "png"
            with open(p, "rb") as f:
                img_part = MIMEImage(f.read(), _subtype=subtype)
            img_part.add_header("Content-ID", f"<{cid}>")
            img_part.add_header("Content-Disposition", "inline", filename=("utf-8", "", p.name))
            related.attach(img_part)
        msg.attach(related)
    else:
        subtype = "html" if html else "plain"
        msg.attach(MIMEText(body, subtype, "utf-8"))

    for fp in attachments:
        _attach_file(msg, fp)

    all_recipients = to + cc

    context = ssl.create_default_context()
    with smtplib.SMTP_SSL(SMTP_HOST, SMTP_PORT, context=context) as client:
        client.login(sender_addr, password)
        client.sendmail(sender_addr, all_recipients, msg.as_string())

    recipient_summary = ", ".join(to)
    if cc:
        recipient_summary += f" (cc: {', '.join(cc)})"
    att_info = f", {len(attachments)} attachment(s)" if attachments else ""
    img_info = f", {len(inline_images)} inline image(s)" if inline_images else ""
    print(f"Email sent successfully to {recipient_summary}{att_info}{img_info}")


def _cmd_setup(args):
    staff_id = os.environ.get("TYCLAW_SENDER_STAFF_ID", "")
    if not staff_id:
        print("Error: TYCLAW_SENDER_STAFF_ID env var required", file=sys.stderr)
        sys.exit(1)
    data = {"address": args.address, "password": args.password}
    save_user_credentials(staff_id, "email", data)
    sync_credential_env("email", data)
    print(f"Email credentials saved for {staff_id}")


def _cmd_clear_credentials(_args):
    staff_id = os.environ.get("TYCLAW_SENDER_STAFF_ID", "")
    if not staff_id:
        print("Error: TYCLAW_SENDER_STAFF_ID env var required", file=sys.stderr)
        sys.exit(1)
    if clear_user_credentials(staff_id, "email"):
        clear_credential_env("email")
        print(f"Email credentials cleared for {staff_id}")
    else:
        print(f"No email credentials found for {staff_id}")


def main():
    # 凭证管理子命令
    if len(sys.argv) >= 2 and sys.argv[1] in ("setup", "clear-credentials"):
        parser = argparse.ArgumentParser(description="Email credential management")
        sub = parser.add_subparsers(dest="_sub")
        p_setup = sub.add_parser("setup", help="Set email credentials")
        p_setup.add_argument("--address", required=True, help="Email address")
        p_setup.add_argument("--password", required=True, help="Third-party client security password")
        sub.add_parser("clear-credentials", help="Clear email credentials")
        args = parser.parse_args()
        if args._sub == "setup":
            _cmd_setup(args)
        elif args._sub == "clear-credentials":
            _cmd_clear_credentials(args)
        return

    parser = argparse.ArgumentParser(description="Send email via enterprise mailbox")
    parser.add_argument("--to", required=True,
                        help="Recipient(s), comma-separated")
    parser.add_argument("--subject", required=True, help="Email subject")
    parser.add_argument("--body", default="", help="Email body text")
    parser.add_argument("--cc", default="",
                        help="CC recipient(s), comma-separated")
    parser.add_argument("--attachment", default="",
                        help="Attachment path(s), comma-separated")
    parser.add_argument("--inline-image", default="",
                        help="Inline image path(s), comma-separated; displayed in email body")
    parser.add_argument("--html", action="store_true",
                        help="Treat body as HTML")
    args = parser.parse_args()

    to_list = [a.strip() for a in args.to.split(",") if a.strip()]
    cc_list = [a.strip() for a in args.cc.split(",") if a.strip()]
    att_list = [a.strip() for a in args.attachment.split(",") if a.strip()]
    img_list = [a.strip() for a in args.inline_image.split(",") if a.strip()]

    if not to_list:
        print("Error: --to must specify at least one recipient",
              file=sys.stderr)
        sys.exit(1)
    if not args.body and not img_list:
        print("Error: --body or --inline-image must be provided",
              file=sys.stderr)
        sys.exit(1)

    send_email(
        to=to_list,
        subject=args.subject,
        body=args.body,
        cc=cc_list if cc_list else None,
        attachments=att_list if att_list else None,
        inline_images=img_list if img_list else None,
        html=args.html,
    )


if __name__ == "__main__":
    main()
