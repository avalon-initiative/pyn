-include .env
export

RUN_DIR := _running
PID_DIR := $(RUN_DIR)/pids
LOG_DIR := $(RUN_DIR)/logs
PID_FILE := $(PID_DIR)/pyn.pid
LOG_FILE := $(LOG_DIR)/pyn.log

.PHONY: dev demo docs-check openapi help build run start stop restart status test test-live fmt fmt-check lint check clean

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
	@echo "  make fmt           auto-format"
	@echo "  make fmt-check     format check, no writes"
	@echo "  make lint          clippy + docs link check"
	@echo "  make openapi       regenerate docs/generated/openapi.json"
	@echo "  make check         fmt-check + lint + test — what CI runs"
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

test:
	cargo test --workspace

test-live:
	cargo test --workspace -- --ignored

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all -- --check

lint:
	cargo clippy --workspace --all-targets -- -D warnings
	python3 scripts/check_links.py

DEMO_ADMIN ?= admin
DEMO_PASSWORD ?= demo-password
PYN_ADDR ?= 127.0.0.1:7878

dev: build
	@$(MAKE) --no-print-directory start PYN_BOOTSTRAP_ADMIN=$(DEMO_ADMIN) PYN_BOOTSTRAP_PASSWORD=$(DEMO_PASSWORD) \
		PYN_REGISTRATION=open PYN_ADDR=$(PYN_ADDR) PYN_CONFIG=scripts/demo.pyn.toml
	@PYN_SERVER=http://$(PYN_ADDR) PYN_BOOTSTRAP_ADMIN=$(DEMO_ADMIN) PYN_BOOTSTRAP_PASSWORD=$(DEMO_PASSWORD) scripts/demo.sh
	@echo
	@echo "server   http://$(PYN_ADDR) ($(if $(PYN_DATABASE_URL),postgres,in-memory); stop with make stop)"
	@echo "sign in  $(DEMO_ADMIN) / $(DEMO_PASSWORD)   alice / alice-password   bob / bob-password"
	@echo "web      cd ../pyn-web && npm run dev"

demo:
	cargo build -p pyn-cli
	scripts/demo.sh

docs-check:
	python3 scripts/check_links.py

openapi:
	PYN_UPDATE_OPENAPI=1 cargo test -p pyn-server --test openapi

check: fmt-check lint test

clean:
	cargo clean
	rm -rf $(RUN_DIR)
