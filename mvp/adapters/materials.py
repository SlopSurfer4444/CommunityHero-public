"""Read-only account material import and isolated local post transcription.

Protocol: one JSON request on stdin, one JSON response on stdout. This module
never constructs the conveyor Store or invokes its CLI/provider commands.
"""
from __future__ import annotations

import hashlib
import json
import re
import sqlite3
import subprocess
import sys
import os
from contextlib import closing
from datetime import datetime, timezone
from pathlib import Path
from urllib.parse import parse_qs, urlencode, urlsplit, urlunsplit


ROOT = Path(__file__).resolve().parents[2]
MVP = Path(__file__).resolve().parents[1]
SOURCE_HOSTS = ("vk.com", "vk.ru", "youtube.com", "youtu.be", "instagram.com", "tiktok.com")
SAFE_ID = re.compile(r"^[A-Za-z0-9_-]{1,200}$")
ACCOUNT_NAMES = {"likeavto": "LikeAvto", "baw-russia": "BAW Russia"}


class AdapterError(Exception):
    def __init__(self, code: str, message: str):
        self.code = code
        super().__init__(message)


def _conveyor_root(env) -> Path:
    values = [env[name] for name in ("COMMUNITYHERO_CONVEYOR_ROOT", "COMMUNITYHERO_CONVEYOR_REPO") if name in env]
    if any(not isinstance(value, str) or not value.strip() or not Path(value).is_absolute() for value in values):
        raise AdapterError("invalid_conveyor_root", "Conveyor root must be absolute")
    if env.get("COMMUNITYHERO_RUNTIME_MODE") == "portable" and not values:
        raise AdapterError("invalid_conveyor_root", "Portable conveyor root is not configured")
    paths = [Path(value).resolve() for value in values]
    if paths and any(value != paths[0] for value in paths[1:]):
        raise AdapterError("conflicting_conveyor_root", "Conveyor root aliases conflict")
    return paths[0] if paths else Path("C:/AIDev/Workspaces/repos/Angry.Space.Auto-symphony").resolve()


CONVEYOR = _conveyor_root(os.environ)
COMMON = CONVEYOR / "configs/commentops-fast/common.json"


def _digest(value: str) -> str:
    return hashlib.sha256(value.encode("utf-8")).hexdigest()[:24]


def _scope_key(account: str, value: str) -> str:
    # Preserve LikeAvto import IDs because its reviewed static manifest is bound
    # to them. New accounts receive a namespace and cannot collide with it.
    return value if account == "likeavto" else f"{account}:{value}"


def _account(req: dict) -> str:
    account = req.get("account")
    if account not in ACCOUNT_NAMES:
        raise AdapterError("account_scope", "Account is not configured")
    return account


def _object_ids(req: dict) -> frozenset[str]:
    values = req.get("accountObjectIds")
    if (not isinstance(values, list) or not values or len(values) > 12
            or any(not isinstance(value, str) or not SAFE_ID.fullmatch(value) for value in values)
            or len(set(values)) != len(values)):
        raise AdapterError("account_scope", "Account object allowlist is unavailable")
    return frozenset(values)


def _material(account: str, kind: str, key: str, title: str, text: str, *, post_key: str = "",
              source_url: str = "", updated_at: str = "") -> dict:
    value = {"id": f"import:{kind}:{_digest(key)}", "title": title, "text": text,
             "kind": kind, "revision": 1, "account": ACCOUNT_NAMES[account],
             "updatedAt": updated_at or datetime.now(timezone.utc).isoformat()}
    if post_key:
        value["postKey"] = post_key
    if source_url:
        value["sourceUrl"] = source_url
    return value


def _public_source(value: object) -> str:
    if not isinstance(value, str) or len(value) > 4096:
        return ""
    try:
        parsed = urlsplit(value)
        host = (parsed.hostname or "").lower().rstrip(".")
        if (parsed.scheme != "https" or not host or parsed.username or parsed.password
                or parsed.port not in (None, 443)
                or not any(host == name or host.endswith("." + name) for name in SOURCE_HOSTS)):
            return ""
        query = ""
        if (host == "youtube.com" or host.endswith(".youtube.com")) and parsed.path == "/watch":
            videos = parse_qs(parsed.query, keep_blank_values=False).get("v", [])
            if len(videos) != 1 or not re.fullmatch(r"[A-Za-z0-9_-]{6,64}", videos[0]):
                return ""
            query = urlencode({"v": videos[0]})
        return urlunsplit(("https", parsed.netloc, parsed.path, query, ""))
    except ValueError:
        return ""


def _public_reference(value: object) -> str:
    """Display a fact citation without leaking URL query credentials."""
    if not isinstance(value, str) or len(value) > 4096:
        return ""
    try:
        parsed = urlsplit(value)
        host = (parsed.hostname or "").lower().rstrip(".")
        if (parsed.scheme != "https" or parsed.username or parsed.password
                or parsed.port not in (None, 443) or not re.fullmatch(r"[a-z0-9.-]+", host)
                or "." not in host or ".." in host or host.endswith((".local", ".internal"))
                or all(part.isdigit() for part in host.split("."))):
            return ""
        return urlunsplit(("https", host, parsed.path, "", ""))
    except ValueError:
        return ""


def _is_youtube_source(source_url: str) -> bool:
    try:
        host = (urlsplit(source_url).hostname or "").lower().rstrip(".")
    except ValueError:
        return False
    return host in {"youtube.com", "youtu.be"} or host.endswith(".youtube.com")


def _remove_download_candidates(work: Path) -> None:
    for path in work.glob("source.*"):
        if path.is_file():
            path.unlink(missing_ok=True)


def _download_youtube_cli(source_url: str, work: Path, *, video: bool,
                          timeout_seconds: float, error_type: type[Exception]) -> Path:
    """Download YouTube through the installed module with a hard deadline.

    The imported conveyor worker suppresses the child exception, so its earlier
    failure cannot be classified after the fact. Its command-line entry point
    succeeded for the affected source and provides one independently bounded
    execution path without starting a second download.
    """
    work.mkdir(parents=True, exist_ok=True)
    _remove_download_candidates(work)
    command = [
        sys.executable, "-m", "yt_dlp", "--ignore-config", "--quiet", "--no-warnings",
        "--no-progress", "--no-playlist", "--socket-timeout", "30", "--retries", "2",
        "--fragment-retries", "2", "--format",
        "best[height<=720]/best" if video else "bestaudio/best",
        "--output", str(work / "source.%(ext)s"), source_url,
    ]
    try:
        completed = subprocess.run(
            command,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            shell=False,
            timeout=max(1.0, float(timeout_seconds)),
            text=True,
            encoding="utf-8",
            errors="replace",
        )
    except subprocess.TimeoutExpired as error:
        _remove_download_candidates(work)
        raise error_type("source_download_timeout") from error
    except OSError as error:
        _remove_download_candidates(work)
        raise error_type("source_download_failed") from error
    if completed.returncode != 0:
        _remove_download_candidates(work)
        detail = str(completed.stderr or "").lower()[-8192:]
        if any(marker in detail for marker in ("sign in", "login required", "authentication",
                                                "cookies", "private video")):
            code = "source_download_failed_auth"
        elif any(marker in detail for marker in ("http error 429", "too many requests",
                                                  "rate limit")):
            code = "source_download_failed_rate_limited"
        elif any(marker in detail for marker in ("video unavailable", "not available",
                                                  "has been removed", "does not exist")):
            code = "source_download_failed_unavailable"
        elif any(marker in detail for marker in ("timed out", "timeout")):
            code = "source_download_timeout"
        else:
            code = "source_download_failed"
        raise error_type(code)
    candidates = [path for path in work.glob("source.*")
                  if path.is_file() and not path.name.endswith((".part", ".ytdl"))]
    if len(candidates) != 1:
        _remove_download_candidates(work)
        raise error_type("source_file_missing")
    return candidates[0]


def _adapter_transcriber_class(base_class: type, error_type: type[Exception]) -> type:
    class AdapterLocalMediaTranscriber(base_class):
        def _download(self, source_url: str, work: Path, *, video: bool = False) -> Path:
            if _is_youtube_source(source_url):
                return _download_youtube_cli(
                    source_url,
                    work,
                    video=video,
                    timeout_seconds=self.download_timeout_seconds,
                    error_type=error_type,
                )
            return super()._download(source_url, work, video=video)

    return AdapterLocalMediaTranscriber


def _configured_path(name: str, fallback: Path) -> Path:
    value = os.environ.get(name)
    if value is None:
        if os.environ.get("COMMUNITYHERO_RUNTIME_MODE") == "portable":
            raise AdapterError("materials_path_missing", f"Portable path {name} is not configured")
        return fallback
    if not value.strip() or not Path(value).is_absolute():
        raise AdapterError("materials_path_invalid", f"Path {name} must be absolute")
    return Path(value).resolve()


def _media_root() -> Path:
    return _configured_path("COMMUNITYHERO_MATERIALS_DATA_DIR", MVP / "data/media")


def _old_connection(account: str) -> sqlite3.Connection:
    database = _configured_path("COMMUNITYHERO_MATERIALS_SOURCE_DB", CONVEYOR / f"data/private/commentops-fast/{account}.sqlite3")
    if account not in ACCOUNT_NAMES or not database.is_file():
        raise AdapterError("materials_source_missing", "Account source database is unavailable")
    db = sqlite3.connect(database.as_uri() + "?mode=ro", uri=True)
    db.row_factory = sqlite3.Row
    db.execute("PRAGMA query_only=ON")
    return db


def _own_database(account: str) -> Path:
    if account not in ACCOUNT_NAMES:
        raise AdapterError("account_scope", "Account is not configured")
    return _media_root() / f"materials-{account}.sqlite3"


def _own_connection(account: str) -> sqlite3.Connection:
    database = _own_database(account)
    database.parent.mkdir(parents=True, exist_ok=True)
    db = sqlite3.connect(database)
    db.row_factory = sqlite3.Row
    db.execute("""CREATE TABLE IF NOT EXISTS media (
        post_key TEXT PRIMARY KEY, source_url TEXT NOT NULL, source_digest TEXT NOT NULL,
        title TEXT NOT NULL, transcript TEXT NOT NULL, ocr_text TEXT NOT NULL,
        updated_at TEXT NOT NULL)""")
    return db


def _safe_card_materials(account: str) -> list[dict]:
    override = os.environ.get("COMMUNITYHERO_ACCOUNT_CARD")
    if override is None and os.environ.get("COMMUNITYHERO_RUNTIME_MODE") == "portable":
        raise AdapterError("materials_card_missing", "Portable account card is not configured")
    if override is not None and (not override.strip() or not Path(override).is_absolute()):
        raise AdapterError("materials_card_invalid", "Account card path must be absolute")
    card_file = Path(override).resolve() if override is not None else CONVEYOR / f"configs/commentops-fast/{account}.json"
    if account not in ACCOUNT_NAMES or not card_file.is_file() or not COMMON.is_file():
        raise AdapterError("materials_card_missing", "Account card is unavailable")
    card = json.loads(card_file.read_text(encoding="utf-8"))
    common = json.loads(COMMON.read_text(encoding="utf-8"))
    if card.get("account") != account:
        raise AdapterError("materials_card_invalid", "Account card does not match")
    entries: list[tuple[str, str, str]] = []
    display = ACCOUNT_NAMES[account]
    for label, source, file in (("Общий стиль", common, COMMON), (display, card, card_file)):
        updated = datetime.fromtimestamp(file.stat().st_mtime, timezone.utc).isoformat()
        tone = source.get("tone")
        if isinstance(tone, str) and tone.strip():
            entries.append((f"{label}: стиль", tone.strip(), updated))
        for index, rule in enumerate(source.get("reply_playbook", []), 1):
            if isinstance(rule, str) and rule.strip():
                entries.append((f"{label}: правило {index}", rule.strip(), updated))
    for name in ("routes", "contact_identity", "community_context"):
        value = card.get(name)
        if isinstance(value, dict) and value:
            entries.append((f"{display}: {name}", json.dumps(value, ensure_ascii=False, indent=2),
                            datetime.fromtimestamp(card_file.stat().st_mtime, timezone.utc).isoformat()))
    if isinstance(card.get("moderation"), str):
        entries.append((f"{display}: модерация", card["moderation"],
                        datetime.fromtimestamp(card_file.stat().st_mtime, timezone.utc).isoformat()))
    return [_material(account, "knowledge", _scope_key(account,f"card:{title}"), title, body, updated_at=updated)
            for title, body, updated in entries]


def _db_materials(account: str) -> list[dict]:
    result: list[dict] = []
    with closing(_old_connection(account)) as db:
        for row in db.execute("""SELECT post_key,claim,source_url,created_at FROM facts
                               WHERE account=? ORDER BY post_key,claim""", (account,)):
            claim = str(row["claim"] or "").strip()
            if claim:
                result.append(_material(account, "knowledge", _scope_key(account,f"fact:{row['post_key']}:{claim}:{row['source_url']}"),
                                        "Сохранённый факт", claim, post_key=row["post_key"],
                                        source_url=_public_reference(row["source_url"]), updated_at=row["created_at"]))
        for row in db.execute("""SELECT post_key,post_title,media_title,clean_transcript,transcript,
                                      ocr_text,source_url,attachment_source_url,updated_at
                               FROM post_media WHERE account=? AND state='ready'
                               ORDER BY post_key""", (account,)):
            key = row["post_key"]
            title = str(row["media_title"] or row["post_title"] or key)
            source = _public_source(row["attachment_source_url"] or row["source_url"])
            transcript = str(row["clean_transcript"] or row["transcript"] or "").strip()
            if transcript:
                result.append(_material(account, "transcript", _scope_key(account,f"legacy:{key}"), f"Видео: {title}", transcript,
                                        post_key=key, source_url=source, updated_at=row["updated_at"]))
            ocr = str(row["ocr_text"] or "").strip()
            if ocr:
                result.append(_material(account, "ocr", _scope_key(account,f"legacy:{key}"), f"Текст в кадре: {title}", ocr,
                                        post_key=key, source_url=source, updated_at=row["updated_at"]))
    return result


def _own_materials(account: str) -> list[dict]:
    result = []
    if not _own_database(account).is_file():
        return result
    with closing(_own_connection(account)) as db:
        for row in db.execute("SELECT * FROM media ORDER BY post_key"):
            result.extend(_material_from_own(account, row))
    return result


def _material_from_own(account: str, row: sqlite3.Row) -> list[dict]:
    key, title = row["post_key"], row["title"]
    common = {"post_key": key, "source_url": row["source_url"], "updated_at": row["updated_at"]}
    result = [_material(account, "transcript", _scope_key(account,f"own:{key}"), f"Видео: {title}", row["transcript"], **common)]
    if row["ocr_text"]:
        result.append(_material(account, "ocr", _scope_key(account,f"own:{key}"), f"Текст в кадре: {title}", row["ocr_text"], **common))
    return result


def materials(req: dict) -> dict:
    account = _account(req)
    _object_ids(req)
    own = _own_materials(account)
    own_keys = {(item["kind"], item["postKey"]) for item in own}
    legacy = [item for item in _db_materials(account)
              if (item["kind"], item.get("postKey")) not in own_keys]
    return {"materials": [*_safe_card_materials(account), *legacy, *own]}


def _post_source(req: dict) -> tuple[str, str, str]:
    account = _account(req)
    object_ids = _object_ids(req)
    post_id = req.get("postId")
    post = req.get("post") or {}
    if not isinstance(post_id, str) or not post_id or len(post_id) > 256 or not isinstance(post, dict):
        raise AdapterError("media_post_invalid", "postId and post object are required")
    if post.get("id") and post["id"] != post_id:
        raise AdapterError("media_post_mismatch", "postId does not match post.id")
    post_key = post.get("postKey") or (post_id[5:] if post_id.startswith("post-") else post_id)
    object_id, separator, local_id = post_key.partition(":") if isinstance(post_key, str) else ("", "", "")
    if (not isinstance(post_key, str) or not separator or object_id not in object_ids
            or not SAFE_ID.fullmatch(object_id) or not SAFE_ID.fullmatch(local_id)
            or post_id not in (post_key, f"post-{post_key}")
            or (post.get("objectId") is not None and post["objectId"] != object_id)):
        raise AdapterError("media_post_invalid", "postKey is invalid")
    # Prefer the persisted account source when this exact post was observed by
    # the conveyor. This lookup never modifies that database.
    with closing(_old_connection(account)) as db:
        row = db.execute("""SELECT attachment_source_url,source_url,media_title,post_title
                            FROM post_media WHERE account=? AND post_key=?""",
                         (account, post_key)).fetchone()
    candidates = []
    if row:
        candidates.append(row["attachment_source_url"])
    candidates.append(post.get("attachmentSourceUrl"))
    attachments = post.get("attachments") or []
    if isinstance(attachments, list):
        for attachment in attachments[:20]:
            if isinstance(attachment, dict):
                candidates.extend((attachment.get("source_url"), attachment.get("sourceUrl"),
                                   attachment.get("url")))
    if row:
        candidates.append(row["source_url"])
    candidates.extend((post.get("sourceUrl"), post.get("url")))
    source = next((safe for candidate in candidates if (safe := _public_source(candidate))), "")
    if not source:
        raise AdapterError("media_source_missing", "No supported public video source URL is available")
    title = str((row["media_title"] or row["post_title"]) if row else
                (post.get("title") or post.get("text") or post_key)).strip()[:240]
    return post_key, source, title or post_key


def media(req: dict) -> dict:
    account = _account(req)
    _object_ids(req)
    post_key, source, title = _post_source(req)
    with closing(_old_connection(account)) as db:
        legacy = db.execute("""SELECT post_key,post_title,media_title,clean_transcript,transcript,
                                     ocr_text,source_url,attachment_source_url,updated_at
                              FROM post_media WHERE account=? AND post_key=? AND state='ready'""",
                            (account, post_key)).fetchone()
    if legacy and (legacy["clean_transcript"] or legacy["transcript"]):
        source = _public_source(legacy["attachment_source_url"] or legacy["source_url"])
        title = str(legacy["media_title"] or legacy["post_title"] or post_key)
        existing = [_material(account, "transcript", _scope_key(account,f"legacy:{post_key}"), f"Видео: {title}",
                              str(legacy["clean_transcript"] or legacy["transcript"]),
                              post_key=post_key, source_url=source, updated_at=legacy["updated_at"])]
        if legacy["ocr_text"]:
            existing.append(_material(account, "ocr", _scope_key(account,f"legacy:{post_key}"), f"Текст в кадре: {title}",
                                      str(legacy["ocr_text"]), post_key=post_key,
                                      source_url=source, updated_at=legacy["updated_at"]))
        return {"materials": existing, "reused": True}
    source_digest = _digest(source)
    with closing(_own_connection(account)) as db:
        cached = db.execute("SELECT * FROM media WHERE post_key=? AND source_digest=?",
                            (post_key, source_digest)).fetchone()
        if cached:
            return {"materials": _material_from_own(account, cached), "reused": True}
    sys.path.insert(0, str(CONVEYOR))
    try:
        from commentops_fast.media import LocalMediaTranscriber, MediaPipelineError
    except ImportError as error:
        raise AdapterError("media_runtime_missing", "Local media runtime is unavailable") from error
    transcriber_type = _adapter_transcriber_class(LocalMediaTranscriber, MediaPipelineError)
    transcriber = transcriber_type(_media_root() / f"scratch/{account}")
    if not transcriber.model.is_file():
        raise AdapterError("media_model_missing", "Local Whisper model is unavailable")
    try:
        # No Store, provider, conveyor command, or old DB mutation. Source URLs
        # are public social post links; direct download URLs are not accepted.
        artifact = transcriber.transcribe({"attachment_source_url": source, "source_url": source})
    except MediaPipelineError as error:
        raise AdapterError(error.code, f"Media processing failed: {error.code}") from error
    except Exception as error:
        raise AdapterError("media_processing_failed", f"Media processing failed: {type(error).__name__}") from error
    transcript = re.sub(r"\s+", " ", artifact.raw_transcript).strip()
    if not transcript:
        raise AdapterError("empty_transcript", "Media produced no transcript")
    updated = datetime.now(timezone.utc).isoformat()
    with closing(_own_connection(account)) as db:
        db.execute("""INSERT INTO media(post_key,source_url,source_digest,title,transcript,ocr_text,updated_at)
                      VALUES(?,?,?,?,?,?,?) ON CONFLICT(post_key) DO UPDATE SET
                      source_url=excluded.source_url,source_digest=excluded.source_digest,title=excluded.title,
                      transcript=excluded.transcript,ocr_text=excluded.ocr_text,updated_at=excluded.updated_at""",
                   (post_key, source, source_digest, title, transcript, artifact.ocr_text, updated))
        db.commit()
        saved = db.execute("SELECT * FROM media WHERE post_key=?", (post_key,)).fetchone()
    return {"materials": _material_from_own(account, saved), "reused": False}


def main() -> None:
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    try:
        req = json.load(sys.stdin)
        if not isinstance(req, dict):
            raise AdapterError("request_invalid", "Request must be an object")
        operation = req.get("operation")
        if operation == "materials":
            value = materials(req)
        else:
            raise AdapterError("operation_unsupported", "Unsupported materials operation")
        print(json.dumps({"ok": True, "result": value}, ensure_ascii=False))
    except AdapterError as error:
        print(json.dumps({"ok": False, "error": {"code": error.code, "message": str(error)}}, ensure_ascii=False))
    except Exception as error:
        print(json.dumps({"ok": False, "error": {"code": "materials_failed",
                                                   "message": type(error).__name__}}, ensure_ascii=False))


if __name__ == "__main__":
    main()
