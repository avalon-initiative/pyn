#!/usr/bin/env bash
# Renders the deploy Compose files with Compose's interpolation rules and checks the web and caddy service facts (needs python3 with PyYAML, not Docker).
set -euo pipefail
exec python3 "$(dirname "${BASH_SOURCE[0]}")/test-compose.py"
