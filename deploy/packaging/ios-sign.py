"""Sign the StillWatch iPhoneOS application bundle on Linux or WSL."""

import argparse
import hashlib
import json
import os
import plistlib
import re
import shutil
import ssl
import subprocess
import tempfile
from datetime import datetime, timezone
from pathlib import Path


BUNDLE_ID = "com.stillwatch.stillwatchCompanion"
PROFILE_SIGNER_OID = "1.2.840.113635.100.6.58"
APPLE_ROOTS = {
    "b0b1730ecbc7ff4505142c49f1295e6eda6bcaed7e2c68c5be91b5a11001f024",
    "c2b9b042dd57830e7d117dac55ac8ae19407d38e41d88f3215bc3a890444a050",
    "63343abfb89a6a03ebb57e9b3f5fa7be7c4f5c756f3017b3a8c488c3653e9179",
}
CERTIFICATE_PEM = r"-----BEGIN CERTIFICATE-----.*?-----END CERTIFICATE-----"
MACHO_MAGICS = {
    b"\xcf\xfa\xed\xfe", b"\xce\xfa\xed\xfe", b"\xfe\xed\xfa\xcf",
    b"\xfe\xed\xfa\xce", b"\xca\xfe\xba\xbe", b"\xca\xfe\xba\xbf",
}


def command(*args, error, pass_fds=()):
    result = subprocess.run(args, capture_output=True, check=False, pass_fds=pass_fds)
    if result.returncode != 0:
        raise ValueError(error)
    return result.stdout


def sha256(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def verified_profile(args, work):
    roots = re.findall(CERTIFICATE_PEM, args.apple_roots.read_text(encoding="ascii"), re.S)
    if not roots or any(
        hashlib.sha256(ssl.PEM_cert_to_DER_cert(root)).hexdigest() not in APPLE_ROOTS
        for root in roots
    ):
        raise ValueError("The trust file must contain only the published Apple root certificates.")
    ca_file = work / "apple-roots.pem"
    ca_file.write_text("\n".join(roots) + "\n", encoding="ascii")
    profile_signer = work / "profile-signer.pem"
    content = command(
        "openssl", "cms", "-verify", "-binary", "-inform", "DER", "-in", args.profile,
        "-CAfile", ca_file, "-no-CApath", "-no-CAstore",
        "-purpose", "any", "-signer", profile_signer,
        error="The provisioning profile failed Apple CMS verification.",
    )
    if profile_signer.read_bytes().count(b"-----BEGIN CERTIFICATE-----") != 1:
        raise ValueError("Expected one Apple provisioning profile signer.")
    signer = command(
        "openssl", "x509", "-in", profile_signer, "-noout", "-subject", "-issuer", "-text",
        "-nameopt", "RFC2253", error="Cannot read the provisioning profile signer.",
    ).decode()
    subject, issuer = (line.split("=", 1)[1].split(",") for line in signer.splitlines()[:2])
    if (
        "CN=Apple iPhone OS Provisioning Profile Signing" not in subject
        or "O=Apple Inc." not in subject
        or "CN=Apple iPhone Certification Authority" not in issuer
        or "O=Apple Inc." not in issuer
        or not re.search(rf"(?m)^\s+{re.escape(PROFILE_SIGNER_OID)}:\s*$", signer)
    ):
        raise ValueError("The profile requires Apple's dedicated provisioning signer.")
    return plistlib.loads(content)


def profile_entitlements(profile, certificate, device, now):
    entitlements = profile["Entitlements"]
    teams = profile["TeamIdentifier"]
    app_id = entitlements["application-identifier"]
    prefix, _, bundle_id = app_id.partition(".")
    if (
        bundle_id != BUNDLE_ID
        or prefix not in profile["ApplicationIdentifierPrefix"]
        or len(teams) != 1
        or "iOS" not in profile["Platform"]
    ):
        raise ValueError("Expected an iOS provisioning profile for the exact StillWatch App ID.")
    devices = profile.get("ProvisionedDevices", [])
    if profile.get("ProvisionsAllDevices") or device.casefold() not in {
        item.casefold() for item in devices
    }:
        raise ValueError("The selected device must be registered in the profile.")
    expiry = profile["ExpirationDate"].replace(tzinfo=timezone.utc)
    if not profile["CreationDate"].replace(tzinfo=timezone.utc) <= now < expiry:
        raise ValueError("The provisioning profile is outside its validity period.")
    if certificate not in profile["DeveloperCertificates"]:
        raise ValueError("The signing certificate is absent from the provisioning profile.")
    development = entitlements.get("get-task-allow", False)
    app = {
        "application-identifier": app_id,
        "com.apple.developer.team-identifier": teams[0],
        "get-task-allow": development,
    }
    groups = entitlements.get("keychain-access-groups", [])
    if app_id in groups or f"{prefix}.*" in groups:
        app["keychain-access-groups"] = [app_id]
    return app, "development" if development else "ad-hoc", expiry, len(devices)


def check_bundle(app):
    info = plistlib.loads((app / "Info.plist").read_bytes())
    if (
        info.get("CFBundleIdentifier") != BUNDLE_ID
        or info.get("CFBundleSupportedPlatforms") != ["iPhoneOS"]
        or info.get("CFBundleExecutable") != "Runner"
        or not (app / "Runner").is_file()
        or not (app / "Frameworks/App.framework/App").is_file()
    ):
        raise ValueError("Expected a StillWatch iPhoneOS release bundle with Flutter AOT.")
    for entry in app.rglob("*"):
        if entry.is_symlink() and (
            not entry.resolve(strict=True).is_relative_to(app)
            or os.path.isabs(entry.readlink())
        ):
            raise ValueError("An application link leaves the bundle.")
        if entry.suffix in {".app", ".appex"} or entry.name == "embedded.mobileprovision":
            raise ValueError("Expected an unsigned application without extensions.")
    return info


def read_certificate(args, work):
    pem = command(
        "openssl", "pkcs12", "-in", args.identity, "-passin",
        f"file:{args.password_file}", "-clcerts", "-nokeys",
        error="Cannot read the signing certificate from the PKCS#12 identity.",
    ).decode()
    certificates = re.findall(CERTIFICATE_PEM, pem, re.S)
    if len(certificates) != 1:
        raise ValueError("Expected one signing certificate in the PKCS#12 identity.")
    certificate = ssl.PEM_cert_to_DER_cert(certificates[0])
    leaf = work / "certificate.cer"
    leaf.write_bytes(certificate)
    end = command(
        "openssl", "x509", "-inform", "DER", "-in", leaf,
        "-noout", "-dates", "-dateopt", "iso_8601",
        error="Cannot read signing certificate validity dates.",
    ).decode().splitlines()
    start, expiry = [
        datetime.fromisoformat(field.partition("=")[2]) for field in end
    ]
    return certificate, start, expiry


def sign_bundle(app, args, entitlement_path):
    # rcodesign 0.29 cannot import all PKCS#12 encryption formats supported by OpenSSL.
    # Hand the decoded key to the signer in memory.
    with os.fdopen(os.memfd_create("stillwatch-signing-key"), "w+b") as pem:
        command(
            "openssl", "pkcs12", "-in", args.identity, "-passin",
            f"file:{args.password_file}", "-noenc", "-out", f"/proc/self/fd/{pem.fileno()}",
            error="Cannot unlock the PKCS#12 signing identity.",
            pass_fds=(pem.fileno(),),
        )
        command(
            "rcodesign", "sign", "-C", "/dev/null", "--timestamp-url", "none",
            "--pem-file", f"/proc/self/fd/{pem.fileno()}",
            "--entitlements-xml-file", entitlement_path, app,
            error="rcodesign could not sign the app and its frameworks.",
            pass_fds=(pem.fileno(),),
        )


def verify_bundle(app):
    for entry in app.rglob("*"):
        if entry.is_file() and not entry.is_symlink():
            with entry.open("rb") as stream:
                is_macho = stream.read(4) in MACHO_MAGICS
            if is_macho:
                command(
                    "rcodesign", "verify", "-C", "/dev/null", entry,
                    error=f"Mach-O signature verification failed for {entry.relative_to(app)}.",
                )
    if not (app / "_CodeSignature/CodeResources").is_file():
        raise ValueError("Missing application resource seal.")


def sign(args):
    info = check_bundle(args.app)
    now = datetime.now(timezone.utc)
    with tempfile.TemporaryDirectory(prefix=".stillwatch-sign-", dir=args.output.parent) as temp:
        work = Path(temp)
        profile = verified_profile(args, work)
        certificate, valid_from, valid_to = read_certificate(args, work)
        if not valid_from <= now < valid_to:
            raise ValueError("The signing certificate is outside its validity period.")
        entitlements, kind, expiry, device_count = profile_entitlements(
            profile, certificate, args.device, now,
        )
        entitlement_path = work / "entitlements.plist"
        entitlement_path.write_bytes(plistlib.dumps(entitlements))
        payload = work / "Payload"
        app = payload / "StillWatch.app"
        shutil.copytree(args.app, app, symlinks=True)
        shutil.copyfile(args.profile, app / "embedded.mobileprovision")
        sign_bundle(app, args, entitlement_path)
        verify_bundle(app)
        staged = work / "StillWatch.ipa"
        subprocess.run(
            ["zip", "-qry", staged, "Payload"], cwd=work, check=True,
        )
        digest = sha256(staged)
        os.link(staged, args.output)
    return {
        "bundle_id": BUNDLE_ID,
        "version": info["CFBundleShortVersionString"],
        "build": info["CFBundleVersion"],
        "signing_type": kind,
        "expires_at": min(expiry, valid_to).isoformat(),
        "eligible_device_count": device_count,
        "profile_sha256": sha256(args.profile),
        "ipa_sha256": digest,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("app", "profile", "identity", "password-file", "apple-roots"):
        parser.add_argument(f"--{name}", required=True, type=lambda path: Path(path).resolve(strict=True))
    parser.add_argument("--device-file", required=True, type=Path, help="File with the target iPhone UDID.")
    parser.add_argument("--output", required=True, type=lambda path: Path(path).absolute())
    args = parser.parse_args()
    try:
        if not hasattr(os, "memfd_create"):
            raise ValueError("Run iOS signing on Linux or WSL.")
        import resource

        os.umask(0o077)
        resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
        args.device = args.device_file.read_text(encoding="ascii").strip()
        if not re.fullmatch(r"[0-9a-fA-F-]{20,40}", args.device):
            raise ValueError("Invalid target iPhone UDID.")
        if args.output.suffix != ".ipa" or args.output.exists():
            raise ValueError("Choose a new output path with the .ipa suffix.")
        print(json.dumps(sign(args), indent=2))
    except (OSError, ValueError, KeyError, plistlib.InvalidFileException, subprocess.CalledProcessError) as error:
        parser.exit(1, f"{error}\n")


if __name__ == "__main__":
    main()
