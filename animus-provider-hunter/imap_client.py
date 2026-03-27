# animus-provider-hunter/imap_client.py
"""IMAP email client for polling verification emails."""
import asyncio
import email
import imaplib
import os
import re
import time

IMAP_HOST = os.environ["ANIMUS_EMAIL_IMAP_HOST"]
IMAP_PORT = int(os.environ.get("ANIMUS_EMAIL_IMAP_PORT", "993"))
EMAIL_ADDRESS = os.environ["ANIMUS_EMAIL_ADDRESS"]
EMAIL_PASSWORD = os.environ["ANIMUS_EMAIL_PASSWORD"]


def _extract_body(msg) -> str:
    if msg.is_multipart():
        parts = []
        for part in msg.walk():
            if part.get_content_type() == "text/plain":
                payload = part.get_payload(decode=True)
                if payload:
                    parts.append(payload.decode("utf-8", errors="replace"))
        return "\n".join(parts)
    payload = msg.get_payload(decode=True)
    return payload.decode("utf-8", errors="replace") if payload else ""


def _extract_verification_link(body: str) -> str | None:
    """Find the first https:// verification URL in the email body."""
    urls = re.findall(r'https?://[^\s<>"\']+', body)
    for url in urls:
        if any(kw in url.lower() for kw in ["verify", "confirm", "activate", "token", "email"]):
            return url
    return urls[0] if urls else None


async def wait_for_verification_email(
    subject_contains: str,
    timeout_seconds: int = 120
) -> tuple[str | None, str | None]:
    """
    Poll Gmail IMAP for an unread email matching subject_contains.
    Returns (verification_link, full_body).
    Raises TimeoutError on timeout.
    """
    deadline = time.time() + timeout_seconds
    while time.time() < deadline:
        try:
            mail = imaplib.IMAP4_SSL(IMAP_HOST, IMAP_PORT)
            mail.login(EMAIL_ADDRESS, EMAIL_PASSWORD)
            mail.select("INBOX")
            _, data = mail.search(None, f'(UNSEEN SUBJECT "{subject_contains}")')
            if data[0]:
                msg_ids = data[0].split()
                _, msg_data = mail.fetch(msg_ids[-1], "(RFC822)")
                msg = email.message_from_bytes(msg_data[0][1])
                body = _extract_body(msg)
                link = _extract_verification_link(body)
                mail.logout()
                return link, body
            mail.logout()
        except Exception as e:
            print(f"[imap] error: {e}", flush=True)
        await asyncio.sleep(5)
    raise TimeoutError(f"No email with subject containing '{subject_contains}' after {timeout_seconds}s")
