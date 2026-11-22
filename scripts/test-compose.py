#!/usr/bin/env python3
"""Renders deploy/docker-compose*.yml the way Compose interpolates them and checks the TLS facts. No Docker needed."""
import os
import re
import sys

import yaml

root = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "deploy")
failures = 0


def check(name, cond):
    global failures
    if cond:
        print(f"ok   {name}")
    else:
        print(f"FAIL {name}", file=sys.stderr)
        failures += 1


def expand(s, env):
    """Compose interpolation: $$, ${V}, ${V:-d}, ${V-d}, ${V:+d}, ${V+d}, ${V:?m}, with nesting in the word."""
    out, i = [], 0
    while i < len(s):
        if s.startswith("$$", i):
            out.append("$")
            i += 2
        elif s.startswith("${", i):
            depth, j = 1, i + 2
            while depth:
                if s.startswith("${", j):
                    depth += 1
                    j += 2
                elif s[j] == "}":
                    depth -= 1
                    j += 1
                else:
                    j += 1
            body = s[i + 2 : j - 1]
            m = re.match(r"([A-Za-z_][A-Za-z0-9_]*)(?:(:?[-+?])(.*))?$", body, re.S)
            name, op, word = m.group(1), m.group(2), m.group(3)
            val = env.get(name)
            if op is None:
                out.append(val or "")
            else:
                colon = op.startswith(":")
                unset = val is None or (colon and val == "")
                kind = op[-1]
                if kind == "-":
                    out.append(expand(word, env) if unset else val)
                elif kind == "+":
                    out.append("" if unset else expand(word, env))
                else:
                    if unset:
                        raise ValueError(expand(word, env))
                    out.append(val)
            i = j
        else:
            out.append(s[i])
            i += 1
    return "".join(out)


def render(node, env):
    if isinstance(node, str):
        return expand(node, env)
    if isinstance(node, list):
        return [render(x, env) for x in node]
    if isinstance(node, dict):
        return {k: render(v, env) for k, v in node.items()}
    return node


def load(name, env):
    with open(os.path.join(root, name)) as f:
        return render(yaml.safe_load(f), env)


base = {
    "PYN_SERVER_VERSION": "v0.1.2",
    "PYN_WEB_VERSION": "v0.1.5",
    "POSTGRES_PASSWORD": "x",
    "PYN_SECRET_KEY": "y",
    "PYN_API_URL": "https://pyn.example.com",
}
old_env = dict(base, PYN_PUBLIC_URL="http://192.168.1.20:8080", PYN_WEB_PUBLISH="0.0.0.0:8080")

for name in ("docker-compose.yml", "docker-compose.web.yml"):
    web = lambda env, name=name: load(name, env)["services"]["web"]
    w = web(old_env)
    check(f"{name}: no PYN_SITE means no TLS settings reach the image", w["environment"]["PYN_SITE"] == "" and w["environment"]["PYN_TLS"] == "")
    check(f"{name}: an existing .env keeps the 8080 mapping", "0.0.0.0:8080:8080" in w["ports"])
    other = [p for p in w["ports"] if not p.endswith(":8080")]
    check(f"{name}: without PYN_SITE ports 80 and 443 are not taken on the host",
          other == ["0.0.0.0::80", "0.0.0.0::443", "0.0.0.0::443/udp"])
    check(f"{name}: every rendered port is well formed", all(re.fullmatch(r"[0-9.]+:[0-9]*:[0-9]+(/udp)?", p) for p in w["ports"]))
    check(f"{name}: default 8080 stays on loopback", "127.0.0.1:8080:8080" in web(base)["ports"])

    t = web(dict(base, PYN_SITE="pyn.example.com"))
    check(f"{name}: PYN_SITE publishes 80, 443 and 443/udp on all interfaces",
          t["ports"][1:] == ["0.0.0.0:80:80", "0.0.0.0:443:443", "0.0.0.0:443:443/udp"])
    check(f"{name}: PYN_SITE and PYN_TLS reach the image", t["environment"]["PYN_SITE"] == "pyn.example.com")
    t = web(dict(base, PYN_SITE="pyn.example.com", PYN_TLS="internal", PYN_WEB_PUBLISH_HTTP="127.0.0.1:8081", PYN_WEB_PUBLISH_TLS="10.0.0.5:8443",
                 PYN_TRUSTED_PROXY_RANGES="172.18.0.2", PYN_ACCESS_LOG="on"))
    check(f"{name}: PYN_WEB_PUBLISH_HTTP and _TLS override the host side",
          t["ports"][1:] == ["127.0.0.1:8081:80", "10.0.0.5:8443:443", "10.0.0.5:8443:443/udp"])
    check(f"{name}: PYN_TLS, proxy ranges and access log pass through",
          t["environment"]["PYN_TLS"] == "internal" and t["environment"]["PYN_TRUSTED_PROXY_RANGES"] == "172.18.0.2"
          and t["environment"]["PYN_ACCESS_LOG"] == "on")

    t = web(base)
    check(f"{name}: certificate volume is a named volume mounted at /data", "pyn-tls:/data" in t["volumes"])
    check(f"{name}: pyn-tls is declared", "pyn-tls" in load(name, base)["volumes"])
    check(f"{name}: unprivileged port sysctl", t["sysctls"] == {"net.ipv4.ip_unprivileged_port_start": 0})
    check(f"{name}: read-only, tmpfs /tmp only, no capabilities, no new privileges",
          t["read_only"] is True and t["tmpfs"] == ["/tmp"] and t["cap_drop"] == ["ALL"] and "no-new-privileges:true" in t["security_opt"])
    check(f"{name}: no nginx remnant", "nginx" not in open(os.path.join(root, name)).read())
    check(f"{name}: token goes through a file secret, not the environment",
          "CF_API_TOKEN" not in t["environment"] and t["environment"]["CF_API_TOKEN_FILE"] == "/run/secrets/cf_api_token"
          and t["secrets"] == ["cf_api_token"])
    check(f"{name}: no token file means the secret is /dev/null", load(name, base)["secrets"]["cf_api_token"]["file"] == "/dev/null")
    check(f"{name}: PYN_CF_TOKEN_FILE selects the token file",
          load(name, dict(base, PYN_CF_TOKEN_FILE="./secrets/cloudflare-token"))["secrets"]["cf_api_token"]["file"] == "./secrets/cloudflare-token")

full = "docker-compose.yml"
check("an existing PYN_PUBLIC_URL passes through unchanged", load(full, old_env)["services"]["server"]["environment"]["PYN_PUBLIC_URL"] == "http://192.168.1.20:8080")
check("PYN_PUBLIC_URL wins over PYN_SITE",
      load(full, dict(base, PYN_SITE="pyn.example.com", PYN_PUBLIC_URL="https://other.example.com"))["services"]["server"]["environment"]["PYN_PUBLIC_URL"]
      == "https://other.example.com")
check("PYN_PUBLIC_URL is derived from PYN_SITE as https",
      load(full, dict(base, PYN_SITE="pyn.example.com"))["services"]["server"]["environment"]["PYN_PUBLIC_URL"] == "https://pyn.example.com")
check("a bare .env gets the localhost default", load(full, base)["services"]["server"]["environment"]["PYN_PUBLIC_URL"] == "http://localhost:8080")
check("PYN_TRUST_FORWARDED_FOR stays on", load(full, base)["services"]["server"]["environment"]["PYN_TRUST_FORWARDED_FOR"] == "true")
check("the server publish and volumes are as before",
      load(full, base)["services"]["server"]["ports"] == ["127.0.0.1:7878:7878"] and "pyn-objects:/data" in load(full, base)["services"]["server"]["volumes"])

with open(os.path.join(root, "docker-compose.server.yml")) as f:
    check("the server-only file has no web, TLS or nginx settings", not re.search(r"PYN_SITE|PYN_TLS|nginx|pyn-web", f.read()))

if failures:
    print(f"{failures} test(s) failed", file=sys.stderr)
    sys.exit(1)
print("compose tests passed")
