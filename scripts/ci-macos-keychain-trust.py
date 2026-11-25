#!/usr/bin/env python3
"""Give a throwaway macOS CI runner keychain trust settings to import.

Creates self-signed CAs and records them for the keychain import check in
crates/rustpy-net/src/system_store.rs (`RUSTPY_TEST_MACOS_TRUSTED_CAS` and
`RUSTPY_TEST_MACOS_UNTRUSTED_CAS`, appended to $GITHUB_ENV):

- one trusted as a root in the admin trust domain (System keychain),
- one denied in the admin trust domain,
- one in the System keychain without trust settings,
- one trusted in the user trust domain (login keychain), when the runner
  allows setting user trust without an authorization dialog.

`security add-trusted-cert` asks for authorization, which a headless runner
cannot give. Under sudo it succeeds for the admin domain on the hosted macOS
runners; where it is refused, the admin trust settings file is written as
root instead, in the format the tool writes. For the user domain it waits
for a dialog (and `security authorizationdb write` is refused), so the user
CA is then left in the login keychain without trust settings.

Run only on a disposable machine: it changes the system's trust settings.
"""
import os
import plistlib
import subprocess
import sys
import datetime

SYSTEM = "/Library/Keychains/System.keychain"
LOGIN = os.path.expanduser("~/Library/Keychains/login.keychain-db")
ADMIN_PLIST = "/Library/Trust Settings/Admin.plist"
TRUST_ROOT, DENY = 1, 3


def run(*args, timeout=60):
    print("+", " ".join(args), flush=True)
    try:
        result = subprocess.run(args, timeout=timeout, capture_output=True, text=True)
    except subprocess.TimeoutExpired:
        print("  timed out (waiting for authorization?)", flush=True)
        return False
    output = (result.stdout + result.stderr).strip()
    if output:
        print("  " + output.replace("\n", "\n  "), flush=True)
    return result.returncode == 0


def make_ca(directory, name):
    pem = os.path.join(directory, name + ".pem")
    subprocess.run(
        ["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "2",
         "-subj", "/CN=RustPy CI " + name,
         "-addext", "basicConstraints=critical,CA:TRUE",
         "-addext", "keyUsage=critical,keyCertSign,cRLSign",
         "-keyout", os.path.join(directory, name + ".key"), "-out", pem],
        check=True, capture_output=True)
    der = subprocess.run(["openssl", "x509", "-in", pem, "-outform", "DER"],
                         check=True, capture_output=True).stdout
    sha1 = subprocess.run(["openssl", "x509", "-noout", "-fingerprint", "-sha1", "-in", pem],
                          check=True, capture_output=True, text=True).stdout
    return pem, der, sha1.split("=", 1)[1].strip().replace(":", "").upper()


def der_element(data, offset):
    """(header length, content length) of the DER element at `offset`."""
    length = data[offset + 1]
    if length < 0x80:
        return 2, length
    count = length & 0x7F
    return 2 + count, int.from_bytes(data[offset + 2:offset + 2 + count], "big")


def issuer_and_serial(der):
    """The issuer Name (full DER) and serial number (content) of a certificate."""
    header, _ = der_element(der, 0)
    tbs = header  # tbsCertificate
    header, _ = der_element(der, tbs)
    item = tbs + header
    if der[item] == 0xA0:  # [0] version
        h, n = der_element(der, item)
        item += h + n
    h, n = der_element(der, item)  # serialNumber
    serial = der[item + h:item + h + n]
    item += h + n
    h, n = der_element(der, item)  # signature algorithm
    item += h + n
    h, n = der_element(der, item)  # issuer
    return der[item:item + h + n], serial


def write_admin_settings(entries):
    """Adds `entries` [(der, sha1, result)] to the admin trust settings file."""
    try:
        with open(ADMIN_PLIST, "rb") as handle:
            settings = plistlib.load(handle)
    except FileNotFoundError:
        settings = {"trustList": {}, "trustVersion": 1}
    now = datetime.datetime.now(datetime.timezone.utc).replace(tzinfo=None)
    for der, sha1, result in entries:
        issuer, serial = issuer_and_serial(der)
        settings["trustList"][sha1] = {
            "issuerName": issuer,
            "serialNumber": serial,
            "modDate": now,
            "trustSettings": [{"kSecTrustSettingsResult": result}],
        }
    data = plistlib.dumps(settings, fmt=plistlib.FMT_XML)
    staging = os.path.join(os.environ.get("RUNNER_TEMP", "/tmp"), "Admin.plist")
    with open(staging, "wb") as handle:
        handle.write(data)
    subprocess.run(["sudo", "mkdir", "-p", os.path.dirname(ADMIN_PLIST)], check=True)
    subprocess.run(["sudo", "install", "-m", "0644", "-o", "root", "-g", "wheel",
                    staging, ADMIN_PLIST], check=True)
    # trustd caches trust settings; it restarts on demand.
    run("sudo", "killall", "trustd")


def exported(admin):
    path = os.path.join(os.environ.get("RUNNER_TEMP", "/tmp"),
                        "export-admin.plist" if admin else "export-user.plist")
    args = ["security", "trust-settings-export"] + (["-d"] if admin else []) + [path]
    if not run(*args):
        return {}
    with open(path, "rb") as handle:
        return plistlib.load(handle).get("trustList", {})


def main():
    directory = os.path.join(os.environ.get("RUNNER_TEMP", "/tmp"), "keychain-cas")
    os.makedirs(directory, exist_ok=True)
    admin = make_ca(directory, "admin-trusted")
    denied = make_ca(directory, "admin-denied")
    plain = make_ca(directory, "untrusted")
    user = make_ca(directory, "user-trusted")

    # Every certificate is in a keychain on the search list.
    for pem, _, _ in (admin, denied, plain):
        if not run("sudo", "security", "add-certificates", "-k", SYSTEM, pem):
            sys.exit("cannot add certificates to the System keychain")

    print("== admin domain through security add-trusted-cert", flush=True)
    tool = (run("sudo", "security", "add-trusted-cert", "-d", "-r", "trustRoot",
                "-k", SYSTEM, admin[0])
            and run("sudo", "security", "add-trusted-cert", "-d", "-r", "deny",
                    "-k", SYSTEM, denied[0]))
    if not tool:
        print("== admin domain: writing the admin trust settings as root", flush=True)
        write_admin_settings([(admin[1], admin[2], TRUST_ROOT), (denied[1], denied[2], DENY)])
    admin_list = exported(True)
    for _, _, sha1 in (admin, denied):
        if sha1 not in admin_list:
            sys.exit(f"admin trust settings do not list {sha1}")
    print(f"admin trust settings set through {'add-trusted-cert' if tool else 'the settings file'}")

    trusted, untrusted = [admin[2]], [denied[2], plain[2]]
    print("== user domain through security add-trusted-cert", flush=True)
    if run("security", "add-trusted-cert", "-r", "trustRoot", "-k", LOGIN, user[0], timeout=15) \
            and user[2] in exported(False):
        trusted.append(user[2])
        print("user trust settings set")
    else:
        print("::warning::user-domain trust settings cannot be set without an "
              "authorization dialog on this runner")
        # Present in the login keychain, but without trust settings.
        run("security", "add-certificates", "-k", LOGIN, user[0])
        untrusted.append(user[2])

    # What the import sees: the keychain search list, whether each
    # certificate is found there, and its exported trust settings.
    run("security", "list-keychains")
    found = subprocess.run(["security", "find-certificate", "-a", "-Z"],
                           capture_output=True, text=True).stdout.upper()
    settings = {**exported(True), **exported(False)}
    for sha1 in trusted + untrusted:
        print(sha1, "in search list:", sha1 in found,
              "trust settings:", settings.get(sha1, {}).get("trustSettings"), flush=True)

    with open(os.environ["GITHUB_ENV"], "a") as env:
        env.write(f"RUSTPY_TEST_MACOS_TRUSTED_CAS={','.join(trusted)}\n")
        env.write(f"RUSTPY_TEST_MACOS_UNTRUSTED_CAS={','.join(untrusted)}\n")
    print("trusted:", trusted, "untrusted:", untrusted)


if __name__ == "__main__":
    main()
