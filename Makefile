-include .env
export

RUN_DIR := _running
PID_DIR := $(RUN_DIR)/pids
LOG_DIR := $(RUN_DIR)/logs
PID_FILE := $(PID_DIR)/pyn.pid
LOG_FILE := $(LOG_DIR)/pyn.log

.PHONY: dev demo docs-check boundary-check openapi help build run start stop restart status test test-live e2e docker-build docker-build-cli fmt fmt-check clippy lint check release clean

help:
	@echo "pyn — local dev commands"
	@echo ""
	@echo "  make build         build the project"
	@echo "  make run           run the server in the foreground (reads .env)"
	@echo "  make dev           build, start a demo server in the background, seed it, print URL and sign-ins"
	@echo "  make demo          seed a running demo server (users, repositories, files, locks)"
	@echo "  make start         run in the background (pid/log under $(RUN_DIR)/)"
	@echo "  make stop          stop what 'make start' started"
	@echo "  make restart       stop, then start"
	@echo "  make status        report whether the background process is running"
	@echo "  make test          run the test suite"
	@echo "  make test-live     also run tests gated on real infra (Postgres)"
	@echo "  make e2e           end-to-end suite: real pyn and pyn-server binaries on temporary repositories"
	@echo "  make docker-build  build the pyn-core server image locally (nothing is pushed)"
	@echo "  make docker-build-cli  build the CLI image locally"
	@echo "  make fmt           auto-format"
	@echo "  make fmt-check     format check, no writes"
	@echo "  make lint          clippy + docs link check + public-boundary check"
	@echo "  make boundary-check  fail on private references, secrets, or a git spawn in runtime source"
	@echo "  make openapi       regenerate docs/generated/openapi.json"
	@echo "  make check         fmt-check + lint + test — what CI runs"
	@echo "  make release       run the release flow: checks, version prompt, bump, commit and tag"
	@echo "  make clean         remove build artifacts and PID/log files"

build:
	cargo build --workspace

run:
	cargo run -p pyn-server

# PID-file background run; only `run` is per-stack.
start:
	@mkdir -p $(PID_DIR) $(LOG_DIR)
	@if [ -f $(PID_FILE) ] && kill -0 "$$(cat $(PID_FILE))" 2>/dev/null; then \
		echo "already running (pid $$(cat $(PID_FILE)))"; \
	else \
		( $(MAKE) run > $(LOG_FILE) 2>&1 & echo $$! > $(PID_FILE) ); \
		sleep 1; \
		echo "started (pid $$(cat $(PID_FILE))), logs: $(LOG_FILE)"; \
	fi

stop:
	@if [ -f $(PID_FILE) ] && kill -0 "$$(cat $(PID_FILE))" 2>/dev/null; then \
		kill "$$(cat $(PID_FILE))"; \
		rm -f $(PID_FILE); \
		echo "stopped"; \
	else \
		echo "not running"; \
		rm -f $(PID_FILE); \
	fi

restart: stop start

status:
	@if [ -f $(PID_FILE) ] && kill -0 "$$(cat $(PID_FILE))" 2>/dev/null; then \
		echo "running (pid $$(cat $(PID_FILE)))"; \
	else \
		echo "not running"; \
	fi

# nextest runs tests in parallel (and skips doctests); NEXTEST_ARGS adds options such as a partition. Plain cargo test is the fallback.
test:
	@if cargo nextest --version >/dev/null 2>&1; then \
		cargo nextest run --workspace $(NEXTEST_ARGS) && cargo test --workspace --doc; \
	else cargo test --workspace; fi

test-live:
	@if cargo nextest --version >/dev/null 2>&1; then \
		cargo nextest run --workspace --run-ignored only && cargo test --workspace --doc -- --ignored; \
	else cargo test --workspace -- --ignored; fi

DOCKER_TAG ?= local
docker-build:
	docker build --target server --build-arg VERSION=$(DOCKER_TAG) --build-arg REVISION=$$(git rev-parse --short HEAD) -t pyn-core:$(DOCKER_TAG) .

docker-build-cli:
	docker build --target cli -t pyn-cli:$(DOCKER_TAG) .

e2e:
	cargo test -p pyn-cli --test e2e --test e2e_second

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all -- --check

clippy:
	cargo clippy --workspace --all-targets -- -D warnings

lint: clippy
	python3 scripts/check_links.py
	python3 scripts/check_public_boundary.py

DEMO_ADMIN ?= root
DEMO_PASSWORD ?= demo-password
DEMO_SETUP_TOKEN ?= demo-setup-token-not-a-secret
PYN_ADDR ?= 127.0.0.1:7878
DEV_WEB_ORIGINS ?= http://localhost:5173,http://127.0.0.1:5173

dev: build
	@$(MAKE) --no-print-directory start PYN_SETUP_TOKEN=$(DEMO_SETUP_TOKEN) \
		PYN_REGISTRATION=open PYN_EMAIL_VERIFICATION=false PYN_ALLOWED_ORIGINS=$(DEV_WEB_ORIGINS) PYN_ADDR=$(PYN_ADDR) PYN_CONFIG=scripts/demo.pyn.toml
	@PYN_SERVER=http://$(PYN_ADDR) PYN_SETUP_TOKEN=$(DEMO_SETUP_TOKEN) DEMO_ADMIN=$(DEMO_ADMIN) DEMO_PASSWORD=$(DEMO_PASSWORD) scripts/demo.sh
	@echo
	@echo "server   http://$(PYN_ADDR) ($(if $(PYN_DATABASE_URL),postgres,in-memory); stop with make stop)"
	@echo "sign in  $(DEMO_ADMIN) / $(DEMO_PASSWORD)   alice / alice-password   bob / bob-password"
	@echo "web      cd ../pyn-web && npm run dev"

demo:
	cargo build -p pyn-cli
	scripts/demo.sh

docs-check:
	python3 scripts/check_links.py

boundary-check:
	python3 scripts/check_public_boundary.py

openapi:
	PYN_UPDATE_OPENAPI=1 cargo test -p pyn-server --test openapi

check: fmt-check lint test

release:
	scripts/release.sh

clean:
	cargo clean
	rm -rf $(RUN_DIR)
