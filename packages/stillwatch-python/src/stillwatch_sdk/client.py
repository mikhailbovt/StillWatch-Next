from __future__ import annotations

import json
import logging
import random
import re
import ssl
import sys
from collections.abc import AsyncIterator, Awaitable, Callable, Iterator
from contextvars import ContextVar
from datetime import UTC, datetime, timedelta
from email.utils import parsedate_to_datetime
from typing import Any, cast
from uuid import uuid4

import anyio
import httpx
from anyio.from_thread import start_blocking_portal
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

from .trust import Enrollment, PinnedCore, normalize, proof, validate_session
from .wire import Camera, Change, CoreError, Cursor, Incident, SseParser, counter, decode_object, problem, timestamp, token, uuid

Json = dict[str, Any]
Save = Callable[[Json], Awaitable[None]]
TOTAL = 12
CAMERA_SETUP = 22
WHEP_OFFER = 28
IDLE = 25
MAX_JSON = 4 * 1024 * 1024


class _RedactWireLogs(logging.Filter):
    def filter(self, record: logging.LogRecord) -> bool:
        # HTTPX emits request URLs and httpcore emits protocol details at DEBUG.
        # Only records belonging to the calling application remain untouched.
        return not _sdk_logging.get()


_sdk_logging: ContextVar[bool] = ContextVar("stillwatch_sdk_wire", default=False)
_filter = _RedactWireLogs()
for _logger in ("httpx", "httpcore.connection", "httpcore.http11", "httpcore.http2", "httpcore.proxy"):
    logging.getLogger(_logger).addFilter(_filter)


class AsyncClient:
    """Async v2 client. The application owns the storage of device credentials."""

    def __init__(
        self,
        endpoint: str | PinnedCore,
        *,
        local_token: str | None = None,
        seed: bytes | None = None,
        credentials: Json | None = None,
        persist: Save | None = None,
    ) -> None:
        self._pin = endpoint if isinstance(endpoint, PinnedCore) else None
        self._endpoint = endpoint.endpoint if isinstance(endpoint, PinnedCore) else normalize(endpoint, loopback=True)
        self._local = token(local_token) if local_token is not None else None
        if self._pin is not None and self._local is not None:
            raise CoreError("credentials")
        self._key = Ed25519PrivateKey.from_private_bytes(seed) if seed is not None and len(seed) == 32 else None
        if (credentials is None) != (seed is None) or (credentials is not None and (self._key is None or self._pin is None or persist is None)):
            raise CoreError("credentials")
        if credentials is not None:
            validate_session(credentials)
        self._session = dict(credentials) if credentials is not None else None
        self._persist = persist
        self._dirty = self._terminal = False
        self._lock = anyio.Lock()
        self._http = httpx.AsyncClient(
            base_url=self._endpoint,
            verify=self._pin.context() if self._pin is not None else True,
            follow_redirects=False,
            trust_env=False,
            headers={"Accept-Encoding": "identity"},
            timeout=httpx.Timeout(IDLE, connect=3, pool=3, write=5),
            limits=httpx.Limits(max_connections=8, max_keepalive_connections=4, keepalive_expiry=20),
        )

    @classmethod
    async def enroll(cls, offer: Json, label: str) -> Enrollment:
        core = PinnedCore.from_offer(offer)
        version = offer.get("api")
        secret = token(offer.get("secret"))
        challenge = uuid(offer.get("challenge"))
        label = label.strip()
        if (not isinstance(version, dict) or version.get("major") != 2 or not 1 <= len(label) <= 80
            or re.search(r"[\x00-\x1f\x7f]", label) or timestamp(offer.get("expires_at")) <= datetime.now(UTC)):
            raise CoreError("credentials")
        key = Ed25519PrivateKey.generate()
        async with cls(core) as client:
            await client.core()
            credentials = await client._object("POST", "/api/v2/pairing/finish", authorized=False, body={
                "challenge": challenge, "secret": secret, "label": label,
                "public_key": key.public_key().public_bytes_raw().hex(),
                "signature": proof(key, "pair", [core.instance, challenge], secret + label),
            })
        validate_session(credentials)
        return Enrollment(core, key.private_bytes_raw(), credentials)

    async def __aenter__(self) -> AsyncClient:
        return self

    async def __aexit__(self, *exc: object) -> None:
        await self.aclose()

    async def aclose(self) -> None:
        await self._http.aclose()

    async def _access(self, rejected: str | None = None) -> str:
        if self._local is not None:
            return self._local
        async with self._lock:
            if self._session is None or self._key is None or self._pin is None or self._persist is None or self._terminal:
                raise CoreError("credentials")
            current = self._session
            refresh = timestamp(current["access_expires_at"]) <= datetime.now(UTC) + timedelta(seconds=15)
            if refresh or current["access_token"] == rejected:
                device = validate_session(current)
                nonce = str(uuid4())
                value = current["refresh_token"]
                signature = proof(self._key, "refresh", [self._pin.instance, device, nonce], value)
                # A lost response can mean the previous refresh token was consumed.
                self._terminal = True
                updated = await self._object("POST", "/api/v2/device-session/refresh", authorized=False, body={
                    "device_id": device, "nonce": nonce, "refresh_token": value, "signature": signature,
                })
                if validate_session(updated) != device:
                    raise CoreError("identity")
                self._session = updated
                self._terminal = False
                self._dirty = True
            if self._dirty:
                await self._persist(dict(self._session))
                self._dirty = False
            return self._session["access_token"]

    async def core(self) -> Json:
        value = await self._object("GET", "/bootstrap/info", authorized=False)
        identity = uuid(value.get("instance_id"))
        version = value.get("version")
        if not isinstance(version, dict) or version.get("major") != 2 or (self._pin and self._pin.instance != identity):
            raise CoreError("identity")
        return value

    async def health(self) -> Json:
        return await self._object("GET", "/health/ready")

    async def status(self) -> Json:
        return await self._object("GET", "/api/v2/status")

    async def semantic_status(self) -> Json:
        return await self._object("GET", "/api/v2/semantic-status")

    async def health_events(self, *, before: str | None = None, limit: int = 100, active_only: bool = False) -> Json:
        params: dict[str, str | int] = {"limit": limit}
        if before is not None:
            params["before"] = counter(before)
        if active_only:
            params["active_only"] = "true"
        return await self._object("GET", "/api/v2/health-events", params=params)

    async def cameras(self, *, after: str | None = None, limit: int = 100) -> Json:
        return await self._object("GET", "/api/v2/cameras", params=_page(uuid(after) if after is not None else None, limit))

    async def camera(self, camera: str) -> Camera:
        return cast(Camera, await self._object("GET", f"/api/v2/cameras/{uuid(camera)}"))

    async def analysis(self, camera: str) -> Json:
        return await self._object("GET", f"/api/v2/cameras/{uuid(camera)}/analysis-status")

    async def probe(
        self, source: Json, masks: list[Json] | None = None, *, camera_id: str | None = None,
    ) -> Json:
        body: Json = {"source": source, "privacy_masks": masks or []}
        if camera_id is not None:
            body["camera_id"] = uuid(camera_id)
        return await self._object("POST", "/api/v2/source-probes", body=body, timeout=CAMERA_SETUP)

    async def capture_devices(self) -> list[Json]:
        return await self._list("/api/v2/capture-devices")

    async def discover_onvif(self) -> list[Json]:
        return await self._list("/api/v2/onvif/discovery", method="POST")

    async def onvif_profiles(self, device_endpoint: str, credential_ref: str | None = None) -> list[Json]:
        return await self._list("/api/v2/onvif/profiles", method="POST", body={
            "device_endpoint": device_endpoint, "credential_ref": credential_ref,
        })

    async def onvif_stream(self, device_endpoint: str, media: str, token: str, credential_ref: str | None = None) -> Json:
        return await self._object("POST", "/api/v2/onvif/stream", body={
            "device_endpoint": device_endpoint, "credential_ref": credential_ref,
            "media": media, "token": token,
        })

    async def upload_replay(self, video: bytes) -> Json:
        if not 0 < len(video) <= 128 * 1024 * 1024:
            raise CoreError("invalid_request")
        _, body = await self._request(
            "POST", "/api/v2/replay-uploads", content=video,
            headers={"Content-Type": "application/octet-stream"}, limit=16 * 1024, timeout=125,
        )
        return decode_object(body)

    async def create_camera(self, config: Json, key: str) -> Camera:
        return cast(Camera, await self._object("POST", "/api/v2/cameras", body=config, key=key, timeout=CAMERA_SETUP))

    async def patch_camera(self, camera: str, patch: Json, key: str, revision: str) -> Camera:
        return cast(Camera, await self._object("PATCH", f"/api/v2/cameras/{uuid(camera)}",
                                               body=patch, key=key, revision=revision, timeout=CAMERA_SETUP))

    async def delete_camera(self, camera: str, key: str, revision: str) -> None:
        await self._request("DELETE", f"/api/v2/cameras/{uuid(camera)}", key=key, revision=revision)

    async def snapshot(self) -> Json:
        value = await self._object("GET", "/api/v2/snapshot")
        Cursor.parse(value.get("cursor"))
        return value

    async def alerts(self) -> Json:
        return await self._object("GET", "/api/v2/alerts")

    async def alert_seen(self, alert: str, cycle: str) -> None:
        await self._request("POST", f"/api/v2/alerts/{counter(alert)}/seen",
                            body={"attention_cycle": counter(cycle)})

    async def incidents(self, *, after: str | None = None, limit: int = 100) -> Json:
        return await self._object("GET", "/api/v2/incidents", params=_page(after, limit))

    async def incident(self, incident: str) -> Incident:
        return cast(Incident, await self._object("GET", f"/api/v2/incidents/{uuid(incident)}"))

    async def acknowledge(self, incident: str, key: str, revision: str) -> Incident:
        return await self._command(f"/incidents/{uuid(incident)}/acknowledgements", {}, key, revision)

    async def resolve(self, incident: str, reason: str, key: str, revision: str) -> Incident:
        return await self._command(f"/incidents/{uuid(incident)}/resolution", {"reason": reason}, key, revision)

    async def feedback(
        self, incident: str, classification: str, reason: str, evidence_version: str, key: str, revision: str,
    ) -> Incident:
        body = {"classification": classification, "reason": reason, "evidence_version": counter(evidence_version)}
        return await self._command(f"/incidents/{uuid(incident)}/feedback", body, key, revision)

    async def silence(self, incident: str, seconds: int, key: str, revision: str) -> Incident:
        return await self._command(f"/alerts/{uuid(incident)}/silence", {"duration_seconds": seconds}, key, revision)

    async def _command(self, path: str, body: Json, key: str, revision: str) -> Incident:
        return cast(Incident, await self._object("POST", f"/api/v2{path}", body=body, key=key, revision=revision))

    async def evidence(self, incident: str, packet: str) -> Json:
        return await self._object("GET", f"/api/v2/incidents/{uuid(incident)}/evidence/{uuid(packet)}")

    async def evidence_image(self, incident: str, packet: str, ordinal: int) -> bytes:
        if type(ordinal) is not int or not 0 <= ordinal <= 255:
            raise CoreError("invalid_request")
        return await self._image(f"/api/v2/incidents/{uuid(incident)}/evidence/{uuid(packet)}/{ordinal}", 512 * 1024)

    async def camera_image(self, camera: str) -> bytes:
        return await self._image(f"/api/v2/cameras/{uuid(camera)}/snapshot.jpg", 1024 * 1024)

    async def _image(self, path: str, limit: int) -> bytes:
        headers, content = await self._request("GET", path, limit=limit)
        if headers.get("content-type") != "image/jpeg" or not content.startswith(b"\xff\xd8\xff"):
            raise CoreError("protocol")
        return content

    async def devices(self) -> list[Json]:
        return await self._list("/api/v2/devices")

    async def issue_pairing(self, role: str, cameras: list[str], scopes: list[str], *, proxy: bool = False) -> Json:
        path = "/api/v2/pairing?profile=proxy" if proxy else "/api/v2/pairing"
        return await self._object("POST", path, body={
            "role": role, "camera_ids": [uuid(value) for value in cameras], "scopes": scopes,
        })

    async def revoke_device(self, device: str) -> None:
        await self._request("DELETE", f"/api/v2/devices/{uuid(device)}")

    async def stream_session(self, camera: str, profile: str = "preview") -> Json:
        result = await self._object("POST", "/api/v2/stream-sessions", body={"camera_id": uuid(camera), "profile": profile})
        path = _whep(result.get("id"))
        if result.get("whep_path") != path or result.get("codec") != "h264" or result.get("redacted") is not True:
            raise CoreError("protocol")
        return result

    async def offer_whep(self, session: str, sdp: str) -> tuple[str, str | None]:
        path = _whep(session)
        headers, content = await self._request(
            "POST", path, content=sdp.encode(), headers={"Content-Type": "application/sdp"},
            limit=48 * 1024, timeout=WHEP_OFFER,
        )

        if headers.get("location") != path or headers.get("content-type") != "application/sdp":
            raise CoreError("protocol")
        return content.decode("utf-8"), headers.get("etag")

    async def trickle(self, session: str, etag: str, fragment: str) -> None:
        await self._request("PATCH", _whep(session), content=fragment.encode(), headers={
            "If-Match": etag, "Content-Type": "application/trickle-ice-sdpfrag",
        })

    async def close_whep(self, session: str) -> None:
        await self._request("DELETE", _whep(session))

    async def _object(self, method: str, path: str, **options: Any) -> Json:
        _, data = await self._request(method, path, **options)
        return decode_object(data)

    async def _list(self, path: str, *, method: str = "GET", body: Json | None = None) -> list[Json]:
        _, data = await self._request(method, path, body=body)
        try:
            values = json.loads(data)
        except (ValueError, RecursionError):
            raise CoreError("protocol") from None
        if not isinstance(values, list) or not all(isinstance(value, dict) for value in values):
            raise CoreError("protocol")
        return values

    async def _send(self, method: str, path: str, **options: Any) -> httpx.Response:
        if not path.startswith("/") or path.startswith("//") or "\\" in path or "#" in path:
            raise CoreError("endpoint")
        marker = _sdk_logging.set(True)
        try:
            request = self._http.build_request(method, path, **options)
            return await self._http.send(request, stream=True)
        except httpx.TimeoutException:
            raise CoreError("deadline") from None
        except (httpx.RequestError, ssl.SSLError):
            raise CoreError("transport") from None
        except (httpx.InvalidURL, ValueError):
            raise CoreError("invalid_request") from None
        finally:
            _sdk_logging.reset(marker)

    async def _request(
        self, method: str, path: str, *, authorized: bool = True, body: Json | None = None,
        content: bytes | None = None, headers: dict[str, str] | None = None, params: dict | None = None,
        key: str | None = None, revision: str | None = None, limit: int = MAX_JSON, timeout: float = TOTAL,
    ) -> tuple[httpx.Headers, bytes]:
        headers = dict(headers or {})
        if body is not None:
            content = json.dumps(body, ensure_ascii=False, allow_nan=False).encode("utf-8")
            headers["Content-Type"] = "application/json"
        if key is not None:
            if not re.fullmatch(r"[A-Za-z0-9_.-]{1,128}", key):
                raise CoreError("invalid_request")
            headers["Idempotency-Key"] = key
        if revision is not None:
            headers["If-Match"] = f'"{counter(revision)}"'
        repeatable = method == "GET" or key is not None
        renewed = False
        try:
            with anyio.fail_after(timeout) as deadline:
                access = await self._access() if authorized else None
                for attempt in range(3):
                    if access is not None:
                        headers["Authorization"] = f"Bearer {access}"
                    try:
                        response = await self._send(method, path, headers=headers, content=content, params=params)
                    except CoreError as error:
                        if not repeatable or error.code != "transport" or attempt != 0:
                            raise
                        await anyio.sleep(0.15)
                        continue
                    try:
                        if response.is_success:
                            return response.headers, await _read(response, limit)
                        delay = _retry_delay(response, attempt)
                        error = problem(response.status_code, await _read(response, 16 * 1024), delay)
                    finally:
                        with anyio.CancelScope(shield=True):
                            await response.aclose()
                    if repeatable and error.status == 401 and access is not None and not renewed and attempt < 2:
                        replacement = await self._access(access)
                        if replacement == access:
                            raise error
                        access, renewed = replacement, True
                        continue
                    if not repeatable or attempt == 2 or not error.reconnectable or anyio.current_time() + delay >= deadline.deadline:
                        raise error
                    await anyio.sleep(delay)
        except TimeoutError:
            raise CoreError("deadline") from None
        raise CoreError("deadline")

    async def _open_changes(self, cursor: Cursor | None) -> httpx.Response:
        try:
            with anyio.fail_after(TOTAL):
                access = await self._access()
                headers = {"Accept": "text/event-stream", "Authorization": f"Bearer {access}"}
                if cursor is not None:
                    headers["Last-Event-ID"] = str(cursor)
                response = await self._send("GET", "/api/v2/changes", headers=headers)
                if response.status_code != 401:
                    return response
                with anyio.CancelScope(shield=True):
                    await response.aclose()
                replacement = await self._access(access)
                if replacement == access:
                    raise CoreError("unauthorized", 401)
                headers["Authorization"] = f"Bearer {replacement}"
                return await self._send("GET", "/api/v2/changes", headers=headers)
        except TimeoutError:
            raise CoreError("deadline") from None

    async def changes(self, cursor: Cursor | None = None) -> AsyncIterator[Change]:
        """Reconnect from the last yielded event. Save its cursor after applying it."""
        failures = 0
        while True:
            delay = None
            try:
                response = await self._open_changes(cursor)
                try:
                    if not response.is_success:
                        delay = _retry_delay(response, failures)
                        raise problem(response.status_code, await _read(response, 16 * 1024), delay)
                    if response.headers.get("content-type", "").split(";", 1)[0] != "text/event-stream":
                        raise CoreError("protocol")
                    parser = SseParser()
                    async for chunk in response.aiter_raw():
                        for event in parser.feed(chunk):
                            if cursor is not None:
                                if event.cursor.generation != cursor.generation:
                                    raise CoreError("cursor_expired")
                                if int(event.cursor.sequence) <= int(cursor.sequence):
                                    continue
                            yield event
                            cursor, failures = event.cursor, 0
                finally:
                    with anyio.CancelScope(shield=True):
                        await response.aclose()
            except httpx.RequestError:
                pass
            except CoreError as error:
                if not error.reconnectable:
                    raise
            failures = min(5, failures + 1)
            await anyio.sleep(delay if delay is not None else 0.2 * 2**failures + random.random() * 0.2)


class Client:
    """Blocking API over the same transport. Open it with a with statement."""

    def __init__(self, endpoint: str | PinnedCore, *, persist: Callable[[Json], None] | None = None, **options: Any) -> None:
        self._endpoint, self._options = endpoint, options
        self._persist = persist
        self._portal_context = self._portal = self._client = None

    @staticmethod
    def enroll(offer: Json, label: str) -> Enrollment:
        with start_blocking_portal() as portal:
            return portal.call(AsyncClient.enroll, offer, label)

    def __enter__(self) -> Client:
        if self._portal is not None:
            raise RuntimeError("Client context is already open")
        manager = start_blocking_portal()
        portal = manager.__enter__()

        async def save(value: Json) -> None:
            assert self._persist is not None
            await anyio.to_thread.run_sync(self._persist, value)

        try:
            self._client = portal.call(lambda: AsyncClient(
                self._endpoint, persist=save if self._persist is not None else None, **self._options,
            ))
        except BaseException:
            manager.__exit__(*sys.exc_info())
            raise
        self._portal_context, self._portal = manager, portal
        return self

    def __exit__(self, *exc: Any) -> None:
        try:
            self._portal.call(self._client.aclose)
        finally:
            self._portal_context.__exit__(*exc)
            self._portal = self._portal_context = self._client = None

    def __getattr__(self, method: str) -> Callable[..., Any]:
        if method.startswith("_") or method == "aclose" or not hasattr(AsyncClient, method):
            raise AttributeError(method)
        if self._portal is None:
            raise RuntimeError("Open the StillWatch Client context first")
        return lambda *args, **kwargs: self._portal.call(lambda: getattr(self._client, method)(*args, **kwargs))

    def changes(self, cursor: Cursor | None = None) -> Iterator[Change]:
        if self._portal is None:
            raise RuntimeError("Open the StillWatch Client context first")
        portal = self._portal
        iterator = self._client.changes(cursor)
        try:
            while True:
                try:
                    yield portal.call(anext, iterator)
                except StopAsyncIteration:
                    break
        finally:
            if self._portal is portal:
                portal.call(iterator.aclose)


async def _read(response: httpx.Response, limit: int) -> bytes:
    data = bytearray()
    try:
        async for chunk in response.aiter_raw():
            if len(data) + len(chunk) > limit:
                raise CoreError("protocol")
            data.extend(chunk)
    except httpx.TimeoutException:
        raise CoreError("deadline") from None
    except httpx.RequestError:
        raise CoreError("transport") from None
    return bytes(data)


def _retry_delay(response: httpx.Response, attempt: int) -> float:
    value = response.headers.get("retry-after")
    if value is not None:
        try:
            return min(3600.0, float(int(value))) if int(value) >= 0 else 0.0
        except (ValueError, OverflowError):
            try:
                date = parsedate_to_datetime(value)
                return min(3600.0, max(0.0, (date - datetime.now(UTC)).total_seconds()))
            except (ValueError, TypeError, OverflowError):
                pass
    return 0.15 * 2**min(attempt, 5)


def _page(after: str | None, limit: int) -> dict[str, str | int]:
    return {"limit": limit} if after is None else {"limit": limit, "after": after}


def _whep(session: str) -> str:
    return f"/api/v2/stream-sessions/{uuid(session)}/whep"
