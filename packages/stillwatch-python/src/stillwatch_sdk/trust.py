from __future__ import annotations

import base64
import binascii
import hashlib
import ipaddress
import re
import ssl
from dataclasses import dataclass, field
from typing import Any
from urllib.parse import urlsplit

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

from .wire import CoreError, timestamp, token, uuid


def normalize(address: str, *, loopback: bool = False) -> str:
    if not isinstance(address, str) or re.search(r"[\x00-\x20\\?#]", address):
        raise CoreError("endpoint")
    try:
        url = urlsplit(address)
        if url.scheme != ("http" if loopback else "https") or url.path not in ("", "/") or not url.hostname or url.username is not None or url.port == 0:
            raise CoreError("endpoint")
        if loopback and not ipaddress.ip_address(url.hostname).is_loopback:
            raise CoreError("endpoint")
    except ValueError:
        raise CoreError("endpoint") from None
    return f"{url.scheme}://{url.netloc}"


@dataclass(frozen=True, slots=True)
class PinnedCore:
    endpoint: str
    instance: str
    fingerprint: str
    der: bytes = field(repr=False)

    def __post_init__(self) -> None:
        object.__setattr__(self, "endpoint", normalize(self.endpoint))
        uuid(self.instance)
        if len(self.der) > 6144 or hashlib.sha256(self.der).hexdigest() != self.fingerprint:
            raise CoreError("endpoint")

    @classmethod
    def from_offer(cls, value: dict[str, Any]) -> PinnedCore:
        encoded = value.get("tls_certificate_der")
        if not isinstance(encoded, str) or not re.fullmatch(r"[A-Za-z0-9_-]{1,8192}", encoded):
            raise CoreError("endpoint")
        try:
            der = base64.urlsafe_b64decode(encoded + "=" * (-len(encoded) % 4))
        except (ValueError, binascii.Error):
            raise CoreError("endpoint") from None
        return cls(value.get("endpoint"), value.get("core_instance_id"), value.get("tls_certificate_sha256"), der)

    def context(self) -> ssl.SSLContext:
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
        context.minimum_version = ssl.TLSVersion.TLSv1_3
        try:
            context.load_verify_locations(cadata=ssl.DER_cert_to_PEM_cert(self.der))
        except ssl.SSLError:
            raise CoreError("endpoint") from None
        return context


@dataclass(frozen=True, slots=True)
class Enrollment:
    core: PinnedCore
    seed: bytes = field(repr=False)
    credentials: dict[str, Any] = field(repr=False)


def validate_session(credentials: dict[str, Any]) -> str:
    device = credentials.get("device")
    if not isinstance(device, dict) or device.get("revoked_at") is not None:
        raise CoreError("credentials")
    token(credentials.get("access_token"))
    token(credentials.get("refresh_token"))
    if timestamp(credentials.get("refresh_expires_at")) <= timestamp(credentials.get("access_expires_at")):
        raise CoreError("credentials")
    return uuid(device.get("id"))


def proof(key: Ed25519PrivateKey, purpose: str, ids: list[str], text: str) -> str:
    prefix = f"stillwatch/{purpose}/v2\0".encode("ascii")
    identifiers = b"".join(bytes.fromhex(uuid(value).replace("-", "")) for value in ids)
    return key.sign(prefix + identifiers + text.encode("utf-8")).hex()
