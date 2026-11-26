"""Exercise Core ONVIF discovery and standard SOAP operations on an isolated fixture."""

from __future__ import annotations

import argparse
import base64
import contextlib
import hashlib
import json
import os
import socket
import ssl
import threading
import uuid
import xml.etree.ElementTree as xml
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from tempfile import TemporaryDirectory

from replay_core import run_core
from rtsps_core import certificates

SOAP = "http://www.w3.org/2003/05/soap-envelope"
DEVICE = "http://www.onvif.org/ver10/device/wsdl"
MEDIA = "http://www.onvif.org/ver10/media/wsdl"
MEDIA2 = "http://www.onvif.org/ver20/media/wsdl"
TYPES = "http://www.onvif.org/ver10/schema"
DISCOVERY = "http://schemas.xmlsoap.org/ws/2005/04/discovery"
ADDRESSING = "http://schemas.xmlsoap.org/ws/2004/08/addressing"
USERNAME = "qa-camera"
PASSWORD = "qa-camera-password"


def element(body, namespace, name):
    return body.findtext(f"{{{namespace}}}{name}")


class CameraService(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def send_xml(self, namespace, content):
        body = (f'<s:Envelope xmlns:s="{SOAP}" xmlns:m="{namespace}" xmlns:t="{TYPES}">'
                f'<s:Body>{content}</s:Body></s:Envelope>').encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/soap+xml")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        doc = xml.fromstring(self.rfile.read(int(self.headers["Content-Length"])))
        body = doc.find(f"{{{SOAP}}}Body")[0]
        if self.path in {"/device", "/legacy"} and body.tag == f"{{{DEVICE}}}GetServices":
            origin = self.server.origin
            services = ((MEDIA, f"{origin}/media"), (MEDIA2, f"{origin}/media2"))
            if self.path == "/legacy":
                self.send_error(500)
                return
            self.send_xml(DEVICE, "<m:GetServicesResponse>" + "".join(
                f"<m:Service><m:Namespace>{namespace}</m:Namespace>"
                f"<m:XAddr>{url}</m:XAddr></m:Service>" for namespace, url in services
            ) + "</m:GetServicesResponse>")
        elif self.path == "/legacy" and body.tag == f"{{{DEVICE}}}GetCapabilities":
            self.send_xml(DEVICE, "<m:GetCapabilitiesResponse><m:Capabilities><t:Media>"
                          f"<t:XAddr>{self.server.origin}/media</t:XAddr>"
                          "</t:Media></m:Capabilities></m:GetCapabilitiesResponse>")
        elif self.path == "/media":
            self.media1(body, doc)
        elif self.path == "/media2":
            self.media2(body)
        else:
            self.send_error(404)

    def media1(self, body, doc):
        wsse = "http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-secext-1.0.xsd"
        wsu = "http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-utility-1.0.xsd"
        token = doc.find(f".//{{{wsse}}}UsernameToken")
        if token is None:
            self.send_error(401)
            return
        nonce = base64.b64decode(element(token, wsse, "Nonce"), validate=True)
        created = element(token, wsu, "Created").encode()
        digest = base64.b64encode(hashlib.sha1(nonce + created + PASSWORD.encode()).digest()).decode()
        if len(nonce) < 16 or element(token, wsse, "Username") != USERNAME or element(token, wsse, "Password") != digest:
            self.send_error(401)
            return
        self.server.authenticated.add("username-token")
        if body.tag == f"{{{MEDIA}}}GetProfiles":
            self.send_xml(MEDIA, '<m:GetProfilesResponse><m:Profiles token="main&amp;day">'
                          '<t:Name>Main video</t:Name><t:VideoEncoderConfiguration/>'
                          '</m:Profiles></m:GetProfilesResponse>')
        elif body.tag == f"{{{MEDIA}}}GetStreamUri" and element(body, MEDIA, "ProfileToken") == "main&day":
            self.send_xml(MEDIA, f'<m:GetStreamUriResponse><m:MediaUri><t:Uri>rtsp://{self.server.interface}:8554/cam/realmonitor?channel=1&amp;subtype=0</t:Uri>'
                          '</m:MediaUri></m:GetStreamUriResponse>')
        else:
            self.send_error(400)

    def media2(self, body):
        challenge = 'Digest realm="StillWatch QA", nonce="fixture-nonce", algorithm=SHA-256, qop="auth"'
        authorization = self.headers.get("Authorization", "")
        if not authorization:
            self.send_response(401)
            self.send_header("WWW-Authenticate", challenge)
            self.end_headers()
            return
        from urllib.request import parse_http_list, parse_keqv_list
        fields = parse_keqv_list(parse_http_list(authorization.removeprefix("Digest ")))

        def sha(value):
            return hashlib.sha256(value.encode()).hexdigest()

        a1 = sha(f"{USERNAME}:StillWatch QA:{PASSWORD}")
        a2 = sha("POST:/media2")
        digest = sha(f"{a1}:fixture-nonce:{fields['nc']}:{fields['cnonce']}:auth:{a2}")
        if fields.get("username") != USERNAME or fields.get("response") != digest:
            self.send_error(401)
            return
        self.server.authenticated.add("http-digest")
        if body.tag == f"{{{MEDIA2}}}GetProfiles":
            assert element(body, MEDIA2, "Type") == "VideoEncoder"
            self.send_xml(MEDIA2, '<m:GetProfilesResponse><m:Profiles token="sub">'
                          '<m:Name>Substream</m:Name><m:Configurations><m:VideoEncoder/>'
                          '</m:Configurations></m:Profiles></m:GetProfilesResponse>')
        elif body.tag == f"{{{MEDIA2}}}GetStreamUri" and element(body, MEDIA2, "ProfileToken") == "sub":
            assert element(body, MEDIA2, "Protocol") == "RTSP"
            self.send_xml(MEDIA2, f'<m:GetStreamUriResponse><m:Uri>rtsps://{self.server.interface}:8322/sub</m:Uri>'
                          '</m:GetStreamUriResponse>')
        else:
            self.send_error(400)


@contextlib.contextmanager
def discovery_responder(interface, endpoint):
    group = "239.255.255.250"
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as listener:
        listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        listener.bind(("", 3702))
        listener.setsockopt(socket.IPPROTO_IP, socket.IP_ADD_MEMBERSHIP,
                            socket.inet_aton(group) + socket.inet_aton(interface))
        listener.settimeout(8)
        failures = []

        def reply():
            try:
                datagram, sender = listener.recvfrom(8192)
                doc = xml.fromstring(datagram)
                header = doc.find(f"{{{SOAP}}}Header")
                action = element(header, ADDRESSING, "Action")
                assert action == f"{DISCOVERY}/Probe"
                message = element(header, ADDRESSING, "MessageID")
                response = (f'<s:Envelope xmlns:s="{SOAP}" xmlns:a="{ADDRESSING}" xmlns:d="{DISCOVERY}">'
                            f'<s:Header><a:Action>{DISCOVERY}/ProbeMatches</a:Action>'
                            f'<a:RelatesTo>{message}</a:RelatesTo></s:Header><s:Body><d:ProbeMatches>'
                            f'<d:ProbeMatch><d:XAddrs>{endpoint}</d:XAddrs></d:ProbeMatch>'
                            '</d:ProbeMatches></s:Body></s:Envelope>').encode()
                with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as source:
                    source.bind((interface, 0))
                    source.sendto(response, sender)
            except Exception as error:
                failures.append(error)

        thread = threading.Thread(target=reply, daemon=True)
        thread.start()
        yield
        thread.join(9)
        assert not failures, failures
        assert not thread.is_alive()


def run(core, worker, interface, multicast, openssl):
    with TemporaryDirectory(prefix="stillwatch-onvif-") as folder, ThreadingHTTPServer((interface, 0), CameraService) as server:
        root = Path(folder)
        tls_args = ()
        if openssl:
            if interface != "127.0.0.1":
                raise ValueError("TLS fixture uses the 127.0.0.1 IP subject alternative name")
            certificates(openssl, root, None)
            context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
            context.load_cert_chain(root / "correct.pem", root / "correct.key")
            server.socket = context.wrap_socket(server.socket, server_side=True)
            tls_args = ("--onvif-ca-file", str(root / "trusted.pem"))
        server.interface = interface
        server.origin = f"{'https' if openssl else 'http'}://{interface}:{server.server_port}"
        server.authenticated = set()
        endpoint = server.origin + "/device"
        legacy = server.origin + "/legacy"
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            credentials = root / "credentials"
            credentials.mkdir()
            sources = root / "files"
            sources.mkdir()
            credential = str(uuid.uuid4())
            file = credentials / f"{credential}.json"
            file.write_text(json.dumps({"username": USERNAME, "password": PASSWORD}))
            if os.name != "nt":
                file.chmod(0o600)
            discovery_args = ("--onvif-interface", interface) if multicast else ()
            with run_core(core, worker, root / "data", sources, root / "core.log",
                          "--onvif-endpoint", endpoint, "--onvif-endpoint", legacy,
                          "--rtsp-peer", f"{interface}:8554", "--camera-credentials-dir", str(credentials),
                          *discovery_args, *tls_args) as (_, client):
                base = "/api/v2/onvif/"
                request = {"device_endpoint": endpoint, "credential_ref": credential}
                assert client.call("POST", base + "profiles", request, authorized=False)[0] == 401
                assert client.call("POST", base + "profiles", request | {"device_endpoint": server.origin + "/media2"})[0] == 400
                if multicast:
                    with discovery_responder(interface, endpoint):
                        status, discovered = client.call("POST", base + "discovery")
                    assert status == 200 and discovered == [{"endpoint": endpoint, "authorized": True}], discovered
                status, profiles = client.call("POST", base + "profiles", request)
                assert status == 200 and {(p['media'], p['token']) for p in profiles} == {("media1", "main&day"), ("media2", "sub")}, profiles
                status, fallback = client.call("POST", base + "profiles", request | {"device_endpoint": legacy})
                assert status == 200 and [p['token'] for p in fallback] == ["main&day"], fallback
                for media, token, port, path, allowed in (
                    ("media1", "main&day", 8554, "/cam/realmonitor?channel=1&subtype=0", True),
                    ("media2", "sub", 8322, "/sub", False),
                ):
                    status, result = client.call("POST", base + "stream", request | {"media": media, "token": token})
                    assert status == 200 and result == {"authorized": allowed, "source": {
                        "type": "rtsp", "peer": f"{interface}:{port}", "stream_path": path,
                        "secure": media == "media2", "transport": "tcp", "credential_ref": credential,
                    }}, result
                assert server.authenticated == {"http-digest", "username-token"}
        finally:
            server.shutdown()
            thread.join(5)
    print(json.dumps({"platform": os.name, "onvif_multicast": multicast, "onvif_https": bool(openssl), "media1": True,
                      "media2": True, "http_digest_sha256": True, "ws_username_token": True,
                      "legacy_capabilities": True, "configured_scope": True}))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--core", type=Path, required=True)
    parser.add_argument("--worker", type=Path, required=True)
    parser.add_argument("--interface", default="127.0.0.1")
    parser.add_argument("--multicast", action="store_true", help="Send WS-Discovery on the specified isolated test interface")
    parser.add_argument("--openssl", type=lambda path: Path(path).resolve(strict=True), help="Use a generated, explicitly trusted HTTPS camera certificate")
    args = parser.parse_args()
    run(args.core.resolve(strict=True), args.worker.resolve(strict=True), args.interface, args.multicast, args.openssl)
