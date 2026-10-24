#!/usr/bin/env python3
"""Install the fixed-target host helper from the verified Companion CI image.

Only the host installer needs Python. The helper is the same Rust executable as
the Companion. Deployment-specific targets are command arguments, never source.
"""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile


def render(args):
    options = ["--socket", args.socket, "--container", args.container, "--config", args.config,
               "--client-uid", str(args.client_uid), "--upstream-url", args.upstream_url]
    if args.public_url:
        options += ["--public-url", args.public_url]
    # systemd quoting, with specifiers escaped. No shell is involved in ExecStart.
    quote = lambda value: '"' + str(value).replace('\\', '\\\\').replace('"', '\\"').replace('%', '%%') + '"'
    unit = "\n".join([
        "[Unit]", "Description=Sub2Ops fixed-target configuration manager",
        "After=docker.service", "Requires=docker.service", "", "[Service]", "Type=simple",
        "ExecStart=/usr/local/libexec/sub2ops-config-host config-host " + " ".join(map(quote, options)),
        "Restart=on-failure", "RestartSec=2", "UMask=0077", "NoNewPrivileges=true",
        "PrivateTmp=true", "TimeoutStopSec=240", "", "[Install]", "WantedBy=multi-user.target", "",
    ])
    parent = str(Path(args.socket).parent)
    compose = {"services": {"sub2api-ops-companion": {
        "environment": {"SUB2API_CONFIG_SOCKET": "/run/sub2ops-config/control.sock"},
        "volumes": [{"type": "bind", "source": parent, "target": "/run/sub2ops-config", "read_only": True}],
    }}}
    return unit, json.dumps(compose, indent=2) + "\n"


def write(path, content, mode):
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(dir=path.parent, delete=False) as f:
        temp = Path(f.name)
        try:
            os.fchmod(f.fileno(), mode)
            f.write(content.encode())
            f.flush()
            os.fsync(f.fileno())
            os.replace(temp, path)
        finally:
            temp.unlink(missing_ok=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", required=True)
    parser.add_argument("--container", required=True)
    parser.add_argument("--config", required=True)
    parser.add_argument("--upstream-url", required=True)
    parser.add_argument("--public-url", default="")
    parser.add_argument("--client-uid", type=int, default=10001)
    parser.add_argument("--socket", default="/run/sub2ops-config/control.sock")
    parser.add_argument("--check", action="store_true", help="Validate arguments and generated definitions without installing")
    args = parser.parse_args()
    if not re.fullmatch(r"[a-zA-Z0-9][a-zA-Z0-9_.-]*", args.container):
        parser.error("invalid container identifier")
    if any("\n" in str(value) or "\r" in str(value) for value in vars(args).values()):
        parser.error("line breaks are not permitted")
    if not Path(args.config).is_absolute() or not Path(args.socket).is_absolute() or Path(args.socket).name != "control.sock":
        parser.error("configuration and socket must be absolute; socket must be control.sock")
    unit, compose = render(args)
    if args.check:
        print("host helper definitions valid")
        return
    if os.geteuid() != 0:
        parser.error("host installation requires root")
    path = Path(args.config)
    if not path.is_file() or path.is_symlink():
        parser.error("configuration must be an existing regular file")
    destination = Path("/usr/local/libexec/sub2ops-config-host")
    destination.parent.mkdir(parents=True, exist_ok=True)
    created = subprocess.check_output(["docker", "create", args.image], text=True).strip()
    temporary = destination.with_name(".sub2ops-config-host-new")
    try:
        subprocess.run(["docker", "cp", f"{created}:/usr/local/bin/sub2ops-server", str(temporary)], check=True)
        temporary.chmod(0o755)
        os.replace(temporary, destination)
    finally:
        temporary.unlink(missing_ok=True)
        subprocess.run(["docker", "rm", created], check=True, stdout=subprocess.DEVNULL)
    write(Path("/etc/systemd/system/sub2ops-config-host.service"), unit, 0o600)
    write(Path("/etc/sub2ops/config-host.compose.json"), compose, 0o600)
    subprocess.run(["systemctl", "daemon-reload"], check=True)
    subprocess.run(["systemctl", "enable", "sub2ops-config-host.service"], check=True, stdout=subprocess.DEVNULL)
    subprocess.run(["systemctl", "restart", "sub2ops-config-host.service"], check=True)
    subprocess.run(["systemctl", "is-active", "--quiet", "sub2ops-config-host.service"], check=True)


if __name__ == "__main__":
    main()
