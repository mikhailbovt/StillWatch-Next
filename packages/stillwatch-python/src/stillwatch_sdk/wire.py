from __future__ import annotations

import json
import re
from dataclasses import dataclass
from datetime import datetime
from typing import Any, NotRequired, TypedDict
from uuid import UUID


class CoreError(Exception):
    """A redacted transport or Core problem. Response details are excluded."""

    def __init__(self, code: str, status: int | None = None, retry_after: float | None = None):
        self.code, self.status, self.retry_after = code, status, retry_after
        super().__init__(code if status is None else f"{code} (HTTP {status})")

    @property
    def reconnectable(self) -> bool:
        return self.code in {"deadline", "transport"} or self.status in {429, 502, 503, 504}


class Camera(TypedDict):
    id: str
    revision: str
    scene_revision: str
    source_epoch: str
    config: dict[str, Any]
    runtime: dict[str, Any]
    live: bool
    scene_review: str
    dimensions: dict[str, int] | None


class AnalysisOrigin(TypedDict):
    model_manifest_sha256: str
    policy_version: str


class Incident(TypedDict):
    id: str
    camera_id: str
    revision: str
    evidence_version: str
    evidence_packet_id: str | None
    attention_cycle: str
    lifecycle: str
    attention: str
    category: str
    assessment: str
    live: bool
    analysis_origin: NotRequired[AnalysisOrigin]


def counter(value: Any) -> str:
    if not isinstance(value, str) or not re.fullmatch(r"0|[1-9][0-9]{0,19}", value) or int(value) > 2**64 - 1:
        raise CoreError("protocol")
    return value


def uuid(value: Any) -> str:
    try:
        parsed = UUID(value)
    except (TypeError, AttributeError, ValueError):
        raise CoreError("protocol") from None
    if str(parsed) != value or parsed.int == 0:
        raise CoreError("protocol")
    return value


@dataclass(frozen=True, slots=True)
class Cursor:
    generation: str
    sequence: str

    def __post_init__(self) -> None:
        uuid(self.generation)
        counter(self.sequence)

    @classmethod
    def parse(cls, value: str) -> Cursor:
        if not isinstance(value, str) or value.count(":") != 1:
            raise CoreError("protocol")
        return cls(*value.split(":"))

    def __str__(self) -> str:
        return f"{self.generation}:{self.sequence}"


@dataclass(frozen=True, slots=True)
class Change:
    cursor: Cursor
    type: str
    data: dict[str, Any]


def decode_object(data: bytes) -> dict[str, Any]:
    try:
        value = json.loads(data.decode("utf-8"))
    except (ValueError, UnicodeError, RecursionError):
        raise CoreError("protocol") from None
    if not isinstance(value, dict):
        raise CoreError("protocol")
    return value


def problem(status: int, data: bytes, delay: float | None = None) -> CoreError:
    try:
        value = decode_object(data)
        code = value.get("code")
        if value.get("status") == status and isinstance(code, str) and re.fullmatch(r"[a-z_]{1,64}", code):
            return CoreError(code, status, delay)
    except CoreError:
        pass
    return CoreError("http", status, delay)


def timestamp(value: Any) -> datetime:
    try:
        result = datetime.fromisoformat(value)
    except (TypeError, ValueError):
        raise CoreError("protocol") from None
    if result.tzinfo is None:
        raise CoreError("protocol")
    return result


def token(value: Any) -> str:
    if not isinstance(value, str) or not re.fullmatch(r"[a-f0-9]{64}", value):
        raise CoreError("credentials")
    return value


class SseParser:
    """Incremental bytes parser with a bound on both lines and records."""

    limit = 64 * 1024

    def __init__(self) -> None:
        self._line = bytearray()
        self._fields: dict[bytes, bytes] = {}
        self._data: list[bytes] = []
        self._size = 0
        self._skip_lf = False

    def feed(self, chunk: bytes):
        for byte in chunk:
            if self._skip_lf:
                self._skip_lf = False
                if byte == 10:
                    continue
            if byte not in (10, 13):
                self._line.append(byte)
                if len(self._line) >= self.limit:
                    raise CoreError("protocol")
                continue
            self._skip_lf = byte == 13
            if not self._line:
                value = self._finish()
                if value is not None:
                    yield value
                continue
            line = bytes(self._line)
            self._line.clear()
            if line.startswith(b":"):
                continue
            self._size += len(line) + 1
            if self._size > self.limit:
                raise CoreError("protocol")
            key, _, field = line.partition(b":")
            field = field.removeprefix(b" ")
            if key == b"data":
                self._data.append(field)
            elif key == b"event" or (key == b"id" and b"\0" not in field):
                self._fields[key] = field

    def _finish(self) -> Change | None:
        fields, data = self._fields, self._data
        self._fields, self._data, self._size = {}, [], 0
        if not data:
            return None
        if fields.get(b"event") == b"resync":
            raise CoreError("cursor_expired")
        try:
            cursor = Cursor.parse(fields.get(b"id", b"").decode("ascii"))
            event = fields.get(b"event", b"").decode("ascii")
        except UnicodeError:
            raise CoreError("protocol") from None
        payload = decode_object(b"\n".join(data))
        if (payload.get("schema_version"), payload.get("stream_generation"), payload.get("sequence"), payload.get("type")) != (
            "1.0", cursor.generation, cursor.sequence, event
        ):
            raise CoreError("protocol")
        uuid(payload.get("entity_id"))
        counter(payload.get("entity_revision"))
        return Change(cursor, event, payload)
