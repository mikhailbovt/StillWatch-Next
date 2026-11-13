"""Exercise the HTTPS profile with a real nginx and a pinned test certificate."""

from __future__ import annotations

import argparse
import contextlib
import hashlib
import http.client
import json
import secrets
import ssl
import subprocess
import urllib.error
from dataclasses import dataclass, field
from pathlib import Path
from tempfile import TemporaryDirectory
from uuid import uuid4

from lan_core import Client as TlsClient
from replay_core import run_core, until
from rtsp_core import check_port, free_port, launch

REPOSITORY = Path(__file__).resolve().parents[2]
DNS_NAME = "localhost.localdomain"
PROXY_TOKEN = "X-StillWatch-Proxy-Token"
CLIENT_ADDRESS = "X-StillWatch-Client-Address"


@dataclass(frozen=True)
class Proxy:
    endpoint: str
    upstream: int
    flags: tuple[str, ...]
    der: bytes
    secret: str = field(repr=False)

    def upstream_status(self, method, path, *, headers=None, body=None):
        connection = http.client.HTTPConnection("127.0.0.1", self.upstream, timeout=4)
        try:
            connection.request(method, path, body, headers={
                "Host": self.endpoint.removeprefix("https://"), **(headers or {}),
            })
            with connection.getresponse() as response:
                return response.status
        finally:
            connection.close()


@contextlib.contextmanager
def proxy_profile(nginx, openssl, root):
    directory = root / "proxy"
    directory.mkdir(mode=0o700)
    port = free_port()
    upstream = free_port()
    while upstream == port:
        upstream = free_port()
    authority = f"{DNS_NAME}:{port}"
    endpoint = f"https://{authority}"
    certificate = directory / "endpoint.pem"
    der = directory / "endpoint.der"
    key = directory / "endpoint.key"
    secret = secrets.token_hex(32)
    token = directory / "token"
    token.write_text(secret + "\n", encoding="ascii")
    token.chmod(0o600)
    subprocess.run([str(openssl), "req", "-x509", "-newkey", "ec",
                    "-pkeyopt", "ec_paramgen_curve:P-256", "-nodes", "-days", "2",
                    "-subj", f"/CN={DNS_NAME}", "-addext", f"subjectAltName=DNS:{DNS_NAME}",
                    "-addext", "basicConstraints=critical,CA:FALSE", "-addext", "extendedKeyUsage=serverAuth",
                    "-keyout", str(key), "-out", str(certificate)], check=True, capture_output=True)
    der.write_bytes(ssl.PEM_cert_to_DER_cert(certificate.read_text(encoding="ascii")))
    include = directory / "upstream-token.conf"
    include.write_text(f'proxy_set_header {PROXY_TOKEN} "{secret}";\n', encoding="ascii")
    include.chmod(0o600)
    config = (REPOSITORY / "deploy/nginx/stillwatch.conf").read_text(encoding="utf-8")
    for before, after in {
        "listen 443 ssl;": f"listen 127.0.0.1:{port} ssl;",
        'if ($http_host != "watch.example.com")': f'if ($http_host != "{authority}")',
        "proxy_set_header Host watch.example.com;": f"proxy_set_header Host {authority};",
        "watch.example.com": DNS_NAME,
        "/etc/nginx/stillwatch/": directory.as_posix() + "/",
        "/var/log/nginx/stillwatch-error.log": (directory / "error.log").as_posix(),
        "127.0.0.1:8766": f"127.0.0.1:{upstream}",
    }.items():
        config = config.replace(before, after)
    (directory / "nginx.conf").write_text(
        "worker_processes 1;\npid nginx.pid;\nerror_log stderr crit;\nevents { worker_connections 128; }\n"
        "http {\nclient_body_temp_path body;\nproxy_temp_path proxy_temp;\n"
        "fastcgi_temp_path fastcgi;\nuwsgi_temp_path uwsgi;\nscgi_temp_path scgi;\n"
        + config + "\n}\n",
        encoding="utf-8",
    )
    settings = directory / "core.json"
    settings.write_text(json.dumps({"https_proxy": {
        "listen": f"127.0.0.1:{upstream}", "endpoint": endpoint,
        "certificate_der_file": str(der), "token_file": str(token),
    }}), encoding="utf-8")
    with launch([str(nginx), "-p", str(directory) + "/", "-c", "nginx.conf",
                 "-g", "daemon off; master_process off;"], directory / "nginx.log"):
        until(lambda: check_port(port))
        yield Proxy(endpoint, upstream, ("--settings-file", str(settings)), der.read_bytes(), secret)


def proxy_arguments(parser, *, required=False):
    parser.add_argument("--nginx", required=required, type=lambda path: Path(path).resolve(strict=True))
    parser.add_argument("--openssl", type=lambda path: Path(path).resolve(strict=True))
    args = parser.parse_args()
    if bool(args.nginx) != bool(args.openssl):
        parser.error("--nginx and --openssl must be supplied together")
    return args


def run(args):
    with TemporaryDirectory(prefix="stillwatch-https-") as folder, contextlib.ExitStack() as stack:
        root = Path(folder)
        data = root / "data"
        sources = root / "sources"
        sources.mkdir()
        lan_port = free_port()
        proxy = stack.enter_context(proxy_profile(args.nginx, args.openssl, root))
        _, local = stack.enter_context(run_core(args.core, args.worker, data, sources, root / "core.log",
                                              "--lan-listen", f"127.0.0.1:{lan_port}", *proxy.flags))
        assert proxy.upstream_status("GET", "/bootstrap/info") == 403
        assert proxy.upstream_status("GET", "/bootstrap/info", headers={
            PROXY_TOKEN: "0" * 64, CLIENT_ADDRESS: "127.0.0.1",
        }) == 403
        assert proxy.upstream_status("GET", "/bootstrap/info", headers={
            PROXY_TOKEN: proxy.secret, CLIENT_ADDRESS: "127.0.0.1, 192.0.2.1",
        }) == 400
        try:
            TlsClient(proxy.endpoint).request("GET", "/bootstrap/info")
        except urllib.error.URLError as error:
            assert isinstance(error.reason, ssl.SSLCertVerificationError)
        else:
            raise AssertionError("HTTPS connection unexpectedly trusted without the pin")
        remote = TlsClient(proxy.endpoint, proxy.der)
        instance = local.call("GET", "/bootstrap/info")[1]["instance_id"]
        assert remote.request("GET", "/bootstrap/info", extra={
            PROXY_TOKEN: "untrusted", CLIENT_ADDRESS: "invalid", "X-Forwarded-For": "192.0.2.1",
        })[1]["instance_id"] == instance
        assert remote.request("GET", "/bootstrap/info", extra={"Origin": proxy.endpoint})[0] == 200
        assert remote.request("GET", "/bootstrap/info", extra={"Origin": "https://elsewhere.invalid"})[0] == 403
        assert remote.request("GET", "/api/v2/status", token=local.token)[0] == 401

        request = {"role": "viewer", "camera_ids": [str(uuid4())], "scopes": ["streams_read"]}
        default = {"role": "owner", "camera_ids": [], "scopes": []}
        _, lan_offer = local.call("POST", "/api/v2/pairing", default)
        _, proxy_offer = local.call("POST", "/api/v2/pairing?profile=proxy", default)
        assert lan_offer["endpoint"] == f"https://127.0.0.1:{lan_port}"
        assert proxy_offer["endpoint"] == proxy.endpoint
        assert lan_offer["core_instance_id"] == proxy_offer["core_instance_id"] == instance
        assert proxy_offer["tls_certificate_sha256"] == hashlib.sha256(proxy.der).hexdigest()
        assert lan_offer["tls_certificate_sha256"] != proxy_offer["tls_certificate_sha256"]
        assert remote.request("POST", "/api/v2/pairing", request)[0] == 401
        upstream = {PROXY_TOKEN: proxy.secret, CLIENT_ADDRESS: "127.0.0.1"}
        assert proxy.upstream_status("POST", "/api/v2/replay-uploads", headers={
            **upstream, "Authorization": f"Bearer {local.token}",
        }) == 404
        address = proxy.endpoint.removeprefix("https://")
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
        context.load_verify_locations(cadata=ssl.DER_cert_to_PEM_cert(proxy.der))
        with contextlib.closing(http.client.HTTPSConnection(address, context=context, timeout=4)) as client:
            client.request("GET", "/bootstrap/info", headers={"Host": "elsewhere.invalid"})
            with client.getresponse() as response:
                assert response.status == 421
                response.read()
            client.request("POST", "/api/v2/pairing/finish", b"0" * (64 * 1024 + 1), {
                "Content-Type": "application/json",
            })
            with client.getresponse() as response:
                assert response.status == 413
    print(json.dumps({"https_terminated_by_nginx": True, "upstream_requires_proxy": True,
                      "core_instance_matches_lan": True, "body_limit_verified": True}))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for flag in ("core", "worker"):
        parser.add_argument(f"--{flag}", required=True, type=lambda path: Path(path).resolve(strict=True))
    run(proxy_arguments(parser, required=True))
