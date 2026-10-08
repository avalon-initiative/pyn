-include .env
export

RUN_DIR := _running
PID_DIR := $(RUN_DIR)/pids
LOG_DIR := $(RUN_DIR)/logs
PID_FILE := $(PID_DIR)/pyn.pid
LOG_FILE := $(LOG_DIR)/pyn.log

.PHONY: demo docs-check help build run start stop restart status test test-live fmt fmt-check lint check clean

help:
	@echo "pyn — local dev commands"
	@echo ""
	@echo "  make build         build the project"
	@echo "  make run           run in the foreground"
	@echo "  make demo          seed a running server with sample files and locks"
	@echo "  make demo          seed a running server with sample files and locks"
	@echo "  make start         run in the background (pid/log under $(RUN_DIR)/)"
	@echo "  make stop          stop what 'make start' started"
	@echo "  make restart       stop, then start"
	@echo "  make status        report whether the background process is running"
	@echo "  make test          run the test suite"
	@echo "  make test-live     also run tests gated on real infra (Postgres)"
	@echo "  make fmt           auto-format"
	@echo "  make fmt-check     format check, no writes"
	@echo "  make lint          clippy + docs link check"
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

demo:
	cargo build -p pyn-cli
	scripts/demo.sh

docs-check:
	python3 scripts/check_links.py

check: fmt-check lint test

clean:
	cargo clean
	rm -rf $(RUN_DIR)
