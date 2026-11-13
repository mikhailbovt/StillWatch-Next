# Remote control and media

## Private VPN

Bind the TLS listener and ICE listeners to the assigned VPN address. The device pairing offer must contain the reachable address. Allow the Core TLS port and the configured ICE ports only on the intended interface and through the VPN policy. The local administrative listener stays on loopback.

The Rust CLI can issue a five-minute owner, operator or viewer offer. The Companion verifies the pinned certificate and registered Core identity before sending the enrollment proof. Check Core connection from Settings and verify actual video separately in a camera viewer. A reachable TLS API alone says nothing about the media path.

## HTTPS reverse proxy

Run nginx and Core on the same trusted host. Give nginx a dedicated DNS name and TLS 1.3 certificate. Generate a self-signed server leaf with a DNS subject alternative name for that host, then export the same certificate as DER for Core. Keep the certificate's private key in nginx's credential directory. Device enrollment supplies the trust anchor and Core instance ID through the same trusted pairing channel as LAN.

~~~sh
umask 077
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -noenc -days 365 \
  -subj /CN=watch.example.com -addext 'subjectAltName=DNS:watch.example.com' \
  -addext 'basicConstraints=critical,CA:FALSE' -addext 'extendedKeyUsage=serverAuth' \
  -keyout endpoint.key -out endpoint.pem
openssl x509 -in endpoint.pem -outform DER -out endpoint.der
openssl rand -hex 32 > proxy.secret
~~~

Add the HTTPS profile to the administrator's Core settings file.

~~~json
{
  "https_proxy": {
    "listen": "127.0.0.1:8766",
    "endpoint": "https://watch.example.com",
    "certificate_der_file": "/etc/stillwatch/endpoint.der",
    "token_file": "/etc/stillwatch/proxy.secret"
  }
}
~~~

Give the Core service account read access to the DER and ownership of its mode `0600` token file. Use equivalent restricted ACLs on Windows. Copy the token value into a private nginx include as `proxy_set_header X-StillWatch-Proxy-Token "HEX_TOKEN";`. Use the supplied configuration in `deploy/nginx/stillwatch.conf` with the selected DNS name and upstream address. Keep the upstream port on loopback. Core checks the loopback peer and a separate proxy credential before accepting the client address written by nginx. The template strips incoming forwarding headers, applies per-client and global connection bounds, and limits control request bodies to 64 KiB. Use the local or VPN administrative profile for replay uploads. The terminating proxy processes device tokens, control requests and the masked evidence it forwards. Restrict access to its process, configuration and logs accordingly.

Use `stillwatch-cli pair --data-dir CORE_DATA --role viewer --camera CAMERA_ID --scope streams-read --proxy` to issue a proxy offer. When both profiles are configured, the local CLI defaults to LAN. Offers created by a paired owner default to the active profile. Both routes retain the same Core instance, storage and authorization. WebRTC connects separately using the configured ICE candidates and TURN. Check control and video from the remote device.

For certificate renewal, provision the new leaf on nginx and Core together, restart Core with the updated DER and issue fresh pairing offers over a trusted channel. Companion operators verify the new fingerprint with the Core owner, then use **Forget Core on this device** in Settings before enrolling with the new offer. Revoke superseded device registrations after successful migration. Preserve the database so existing camera IDs and history remain available.

## Outbound control relay

Use this profile when Companion reaches a relay host and the Core host can initiate outbound TCP. Build `stillwatch-gateway` with `cargo build --release --locked -p stillwatch-gateway` and run its two modes as separate unprivileged processes. One relay instance serves one Core installation.

Keep the reverse-proxy setup on the Core host and change nginx's TLS listener to `127.0.0.1:9443`. Its certificate and the Core's configured `https_proxy.endpoint` still describe the Companion-facing DNS origin. For the commands below that origin is `https://watch.example.com:8443`. The `Host` check and the upstream `proxy_set_header Host` must include `:8443`. Point the DNS record at the relay, and keep nginx's endpoint private key on the Core host.

On the relay create a separate identity for the connector TLS endpoint with a matching DNS SAN and server authentication purpose. Export the certificate as DER and the private key as unencrypted PKCS#8 DER. Generate a new connector token with `openssl rand -hex 32 > connector.token` using `umask 077`. Transfer the token and connector certificate to the Core host over a trusted channel. The connector matches the relay certificate before sending this token. Protect the key and token with owner-only permissions or ACLs. The two hosts use a connector token distinct from the Core and nginx credentials.

~~~sh
stillwatch-gateway serve --listen 0.0.0.0:8443 --connector-listen 0.0.0.0:9444 \
  --certificate-der /etc/stillwatch/connector.der \
  --private-key-der /etc/stillwatch/connector-key.der \
  --token-file /etc/stillwatch/connector.token
~~~

Run the connector on the Core host. Replace the example address with the relay's reachable address and restrict access to the connector port to the Core site's egress IP where possible. Start both processes through their host service supervisors so a process restart also reloads credentials.

~~~sh
stillwatch-gateway connect --relay 192.0.2.10:9444 --relay-name relay.example.com \
  --relay-certificate-der /etc/stillwatch/connector.der \
  --token-file /etc/stillwatch/connector.token --upstream 127.0.0.1:9443
~~~

Issue a standard proxy pairing offer. The gateway relays the inner TLS bytes to nginx on the Core host, which terminates Companion TLS and applies the same origin, body size and proxy credential checks. The gateway has the connector credential and observes TCP peers, timing and encrypted traffic sizes. The nginx instance sees the connector as the TCP peer, so its IP-based request limit is shared across viewers for this profile. The gateway allows up to 32 simultaneous public connections overall, 12 from an IP and 80 new public connections per one-second window. The default connector pool supplies 24 connections and can be sized for the operator's workload. Each assigned connection has a 35-minute lifetime. The connector replaces lost connections automatically. Reconnecting clients use the Core's normal snapshot and SSE recovery.

Stop both services during connector credential rotation to close existing tunnels, replace the credential files, then restart both. Media uses the Core's separate ICE/TURN setup. Check video and control independently from the remote network.

## TURN

An operator can supply a dedicated Coturn server with shared-secret authentication and an allocation quota. Point Core at a private file containing the Coturn secret, with owner-only permissions. The two flags are shown below.

~~~text
--media-turn turns:turn.example.net:5349?transport=tcp
--media-turn-secret-file /etc/stillwatch/turn.secret
~~~

The stream grant carries a per-session username and HMAC credential expiring after forty minutes. The server closes the WHEP session after thirty minutes at the latest. Session close and device revoke remove the media reader. Coturn must enforce its own allocation lifetimes, bandwidth limits, relay ports and peer restrictions.

For Coturn, set realm, certificate and private key, listening address, external address mapping where needed, allowed relay peers, a low per-user quota and a deployment-wide allocation and bandwidth limit. Enable shared-secret auth and disable anonymous access. Companion provides the short-lived grant credentials directly to libwebrtc before generating the offer.

## Independent heartbeat

Enroll the alarm agent with a separate scoped viewer identity on a different host that can reach the Core. Start the monitor under that host's service supervisor and collect its JSON output. It polls pinned Core readiness, reports state changes, and debounces transient failures.

~~~text
stillwatch-alarm-agent monitor --state MONITOR_STATE --interval-seconds 10 --consecutive-failures 3
~~~

An unreachable state means the observer lost its control path. Check the Core, host network and VPN before diagnosing the cause. Verify video delivery independently.

Public DNS and external NAT qualification require an operator-supplied network.
