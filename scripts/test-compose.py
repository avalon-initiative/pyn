#!/usr/bin/env python3
"""Renders deploy/docker-compose*.yml the way Compose interpolates them and checks the web and caddy service facts. No Docker needed."""
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
removed = re.compile(r"PYN_SITE|PYN_TLS|PYN_TRUSTED_PROXY_RANGES|CF_API_TOKEN|CF_TOKEN|pyn-tls|secrets:|sysctls|ip_unprivileged_port_start")

for name, plain in (("docker-compose.yml", {"db", "server", "web"}), ("docker-compose.web.yml", {"web"})):
    doc = load(name, base)
    svc = doc["services"]
    web = lambda env, name=name: load(name, env)["services"]["web"]
    text = open(os.path.join(root, name)).read()

    check(f"{name}: without the caddy profile the stack is exactly the plain one",
          {k for k, v in svc.items() if "profiles" not in v} == plain and "caddy" in svc)
    check(f"{name}: only the caddy service has a profile, and it is caddy", {k: v["profiles"] for k, v in svc.items() if "profiles" in v} == {"caddy": ["caddy"]})
    check(f"{name}: no service outside the profile publishes 80 or 443",
          not any(re.search(r":(80|443)(/udp)?$", p) for k, v in svc.items() if "profiles" not in v for p in v.get("ports", [])))

    w = web(old_env)
    check(f"{name}: web is plain HTTP on 8080 and reads only PYN_API_URL", set(w["environment"]) == {"PYN_API_URL"})
    check(f"{name}: an existing .env keeps the 8080 mapping", w["ports"] == ["0.0.0.0:8080:8080"])
    check(f"{name}: default 8080 stays on loopback", web(base)["ports"] == ["127.0.0.1:8080:8080"])
    t = web(base)
    check(f"{name}: web is read-only with the nginx tmpfs entries, no capabilities and no new privileges",
          t["read_only"] is True and t["tmpfs"] == ["/tmp", "/etc/nginx/conf.d:uid=101,gid=101"] and t["cap_drop"] == ["ALL"]
          and "no-new-privileges:true" in t["security_opt"])
    check(f"{name}: web has no volumes, secrets or sysctls", not ({"volumes", "secrets", "sysctls"} & set(t)))
    check(f"{name}: no TLS setting remains in the file", not removed.search(text))
    check(f"{name}: the image is nginx's: the conf.d tmpfs is there", "/etc/nginx/conf.d" in text)

    c = load(name, base)["services"]["caddy"]
    check(f"{name}: caddy defaults to a pinned official image with a digest", re.fullmatch(r"caddy:2\.[0-9]+@sha256:[0-9a-f]{64}", c["image"]) is not None)
    check(f"{name}: PYN_CADDY_IMAGE overrides the image",
          load(name, dict(base, PYN_CADDY_IMAGE="registry.example/caddy-cf:2"))["services"]["caddy"]["image"] == "registry.example/caddy-cf:2")
    check(f"{name}: caddy publishes 80, 443 and 443/udp on all interfaces", c["ports"] == ["0.0.0.0:80:80", "0.0.0.0:443:443", "0.0.0.0:443:443/udp"])
    c2 = load(name, dict(base, PYN_WEB_PUBLISH_HTTP="127.0.0.1:8081", PYN_WEB_PUBLISH_TLS="10.0.0.5:8443"))["services"]["caddy"]
    check(f"{name}: PYN_WEB_PUBLISH_HTTP and _TLS override the host side", c2["ports"] == ["127.0.0.1:8081:80", "10.0.0.5:8443:443", "10.0.0.5:8443:443/udp"])
    check(f"{name}: every caddy port is well formed", all(re.fullmatch(r"[0-9.]+:[0-9]+:[0-9]+(/udp)?", p) for p in c["ports"]))
    check(f"{name}: caddy mounts the Caddyfile read-only and keeps /data and /config in named volumes",
          c["volumes"] == ["./Caddyfile:/etc/caddy/Caddyfile:ro", "caddy-data:/data", "caddy-config:/config"]
          and {"caddy-data", "caddy-config"} <= set(doc["volumes"]))
    check(f"{name}: caddy keeps NET_BIND_SERVICE for the binary's file capability", c["cap_drop"] == ["ALL"] and c["cap_add"] == ["NET_BIND_SERVICE"])
    check(f"{name}: caddy has no sysctls and no privileged mode", "sysctls" not in c and not c.get("privileged"))
    check(f"{name}: caddy restarts, rotates logs, waits for web and has a health check",
          c["restart"] == "unless-stopped" and c["logging"]["options"]["max-size"] == "10m" and c["depends_on"] == ["web"] and "healthcheck" in c)

full = "docker-compose.yml"
server = load(full, base)["services"]["server"]
check("PYN_PUBLIC_URL comes from .env only, not from the Compose file", "PYN_PUBLIC_URL" not in server["environment"])
check("PYN_TRUST_FORWARDED_FOR stays on", server["environment"]["PYN_TRUST_FORWARDED_FOR"] == "true")
check("the server publish and volumes are as before",
      server["ports"] == ["127.0.0.1:7878:7878"] and "pyn-objects:/data" in server["volumes"])
check("the full file's volumes are the database, objects and caddy's", set(load(full, base)["volumes"]) == {"pyn-db", "pyn-objects", "caddy-data", "caddy-config"})

for example in (".env.example", ".env.web.example"):
    with open(os.path.join(root, example)) as f:
        txt = f.read()
    check(f"{example}: no TLS-mode settings remain", not re.search(r"PYN_TLS|PYN_TRUSTED_PROXY_RANGES|CF_TOKEN|PYN_ACCESS_LOG|pyn-tls", txt))
    check(f"{example}: the Caddy settings are present and commented", "# COMPOSE_PROFILES=caddy" in txt and "# PYN_CADDY_IMAGE=" in txt)

with open(os.path.join(root, "docker-compose.server.yml")) as f:
    check("the server-only file has no web, caddy or nginx settings", not re.search(r"PYN_SITE|caddy|nginx|pyn-web", f.read()))

if failures:
    print(f"{failures} test(s) failed", file=sys.stderr)
    sys.exit(1)
print("compose tests passed")
