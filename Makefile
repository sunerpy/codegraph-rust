SHELL := /bin/sh
.DEFAULT_GOAL := help

PROJECT_NAME := codegraph-rs
BINARY_NAME := codegraph
CLI_CRATE := codegraph-rs
CARGO ?= cargo
TARGET_DIR := target
DIST_DIR := dist
TARGET ?=
OXFMT_VERSION := 0.64.0
ACTIONLINT_VERSION := 1.7.12
OXFMT_ARGS := --no-error-on-unmatched-pattern --ignore-path .oxfmtignore .

.PHONY: all build build-dev build-prod release release-target install uninstall \
        fmt fmt-rust fmt-oxfmt fmt-check fmt-rust-check fmt-oxfmt-check \
        tools-check workflow-lint docs-check workspace-version typecheck lint test guardrail script-tests archive-smoke \
        check ci pre-ci ui ui-check hooks setup-hooks clean size-compare help \
        coverage coverage-html coverage-lcov coverage-open coverage-clean

all: build

build: build-dev

build-dev:
	@echo "Building $(PROJECT_NAME) (debug)..."
	$(CARGO) build --locked -p $(CLI_CRATE)
	@mkdir -p $(DIST_DIR)
	@if [ -f "$(TARGET_DIR)/debug/$(BINARY_NAME)" ]; then \
		cp "$(TARGET_DIR)/debug/$(BINARY_NAME)" "$(DIST_DIR)/$(BINARY_NAME)"; \
	elif [ -f "$(TARGET_DIR)/debug/$(BINARY_NAME).exe" ]; then \
		cp "$(TARGET_DIR)/debug/$(BINARY_NAME).exe" "$(DIST_DIR)/$(BINARY_NAME).exe"; \
	else echo "binary not found"; exit 1; fi

build-prod: release

release:
	@echo "Building $(PROJECT_NAME) (release)..."
	$(CARGO) build --locked --release -p $(CLI_CRATE)
	@mkdir -p $(DIST_DIR)
	@if [ -f "$(TARGET_DIR)/release/$(BINARY_NAME)" ]; then \
		cp "$(TARGET_DIR)/release/$(BINARY_NAME)" "$(DIST_DIR)/$(BINARY_NAME)"; \
	elif [ -f "$(TARGET_DIR)/release/$(BINARY_NAME).exe" ]; then \
		cp "$(TARGET_DIR)/release/$(BINARY_NAME).exe" "$(DIST_DIR)/$(BINARY_NAME).exe"; \
	else echo "binary not found"; exit 1; fi

release-target:
ifndef TARGET
	$(error TARGET is not set. Usage: make release-target TARGET=x86_64-unknown-linux-musl)
endif
	$(CARGO) build --locked --release -p $(CLI_CRATE) --target "$(TARGET)"
	@mkdir -p $(DIST_DIR)
	@if [ -f "$(TARGET_DIR)/$(TARGET)/release/$(BINARY_NAME)" ]; then \
		cp "$(TARGET_DIR)/$(TARGET)/release/$(BINARY_NAME)" "$(DIST_DIR)/$(BINARY_NAME)-$(TARGET)"; \
	elif [ -f "$(TARGET_DIR)/$(TARGET)/release/$(BINARY_NAME).exe" ]; then \
		cp "$(TARGET_DIR)/$(TARGET)/release/$(BINARY_NAME).exe" "$(DIST_DIR)/$(BINARY_NAME)-$(TARGET).exe"; \
	else echo "binary not found for $(TARGET)"; exit 1; fi

install: release
	$(CARGO) install --locked --path crates/codegraph-cli

uninstall:
	$(CARGO) uninstall $(CLI_CRATE) || true

# Every formatter/linter target fails closed when its tool is absent. CI installs
# the exact pinned versions before invoking the same make check entry point.
tools-check:
	@command -v jq >/dev/null 2>&1 || { echo "jq is required"; exit 1; }
	@command -v python3 >/dev/null 2>&1 || { echo "python3 is required"; exit 1; }
	@python3 -c 'import yaml' >/dev/null 2>&1 || { echo "PyYAML is required"; exit 1; }
	@command -v oxfmt >/dev/null 2>&1 || { echo "oxfmt $(OXFMT_VERSION) is required: npm install --global oxfmt@$(OXFMT_VERSION)"; exit 1; }
	@test "$$(oxfmt --version | awk '{print $$NF}')" = "$(OXFMT_VERSION)" \
		|| { echo "oxfmt $(OXFMT_VERSION) is required: npm install --global oxfmt@$(OXFMT_VERSION)"; exit 1; }
	@command -v actionlint >/dev/null 2>&1 || { echo "actionlint $(ACTIONLINT_VERSION) is required"; exit 1; }
	@test "$$(actionlint -version | head -1 | sed 's/^v//')" = "$(ACTIONLINT_VERSION)" \
		|| { echo "actionlint $(ACTIONLINT_VERSION) is required"; exit 1; }
	@command -v shellcheck >/dev/null 2>&1 || { echo "shellcheck is required"; exit 1; }

fmt: tools-check fmt-rust fmt-oxfmt
	@echo "Formatting complete."

fmt-rust:
	$(CARGO) fmt --all

fmt-oxfmt:
	oxfmt --write $(OXFMT_ARGS)

fmt-check: tools-check fmt-rust-check fmt-oxfmt-check
	@echo "Format check complete."

fmt-rust-check:
	$(CARGO) fmt --all --check

fmt-oxfmt-check:
	oxfmt --check $(OXFMT_ARGS)

workspace-version:
	@bash scripts/check-workspace-versions.sh

typecheck:
	$(CARGO) check --workspace --locked

lint:
	$(CARGO) clippy --workspace --all-targets --locked -- -D warnings

test:
	$(CARGO) test --workspace --locked

workflow-lint: tools-check
	actionlint .github/workflows/*.yml
	shellcheck .github/scripts/*.sh .githooks/pre-push scripts/*.sh scripts/tests/*.sh

docs-check: tools-check
	python3 scripts/docs-check.py

# Guardrail includes deterministic workflow/installer contract checks.
guardrail:
	@bash scripts/guardrail.sh

script-tests:
	@for test_script in scripts/tests/*.test.sh; do \
		echo "Running $$test_script"; bash "$$test_script"; \
	done

# Keep all supported local/CI entry points byte-for-byte equivalent in ordering.
# The version check is the first Cargo subprocess and every later Cargo command
# is locked. Recursive invocations make the ordering explicit even under make -j.
check:
	@$(MAKE) --no-print-directory workspace-version
	@$(MAKE) --no-print-directory tools-check
	@$(MAKE) --no-print-directory fmt-check
	@$(MAKE) --no-print-directory docs-check
	@$(MAKE) --no-print-directory workflow-lint
	@$(MAKE) --no-print-directory lint
	@$(MAKE) --no-print-directory test
	@$(MAKE) --no-print-directory release
	@$(MAKE) --no-print-directory guardrail
	@$(MAKE) --no-print-directory script-tests
	@echo "All checks passed."

ci: check
pre-ci: check ui-check archive-smoke

# Rebuild the embedded viewer bundle (crates/codegraph-ui/viewer) from ui/.
ui:
	cd ui && npm ci && npm run build

# The browser viewer's frontend (`ui/`, Node + npm): install from the lockfile,
# type-check, test, rebuild, and require the committed bundle under
# crates/codegraph-ui/viewer to be exactly that build. CI runs it as its own job.
ui-check:
	cd ui && npm ci && npm run check && npm test && npm run build
	git diff --exit-code -- crates/codegraph-ui/viewer
	@stale="$$(git status --porcelain --untracked-files=all -- crates/codegraph-ui/viewer)"; \
		if [ -n "$$stale" ]; then \
			echo "the committed viewer bundle is not this build; commit crates/codegraph-ui/viewer:"; \
			echo "$$stale"; exit 1; \
		fi

archive-smoke:
	bash scripts/smoke-release-archive.sh

LLVM_COV_INSTALL := Install with: cargo install cargo-llvm-cov --version 0.8.7 --locked
LLVM_COV_HTML := $(TARGET_DIR)/llvm-cov/html/index.html

define REQUIRE_LLVM_COV
	@command -v cargo-llvm-cov >/dev/null 2>&1 || { echo "cargo-llvm-cov not found. $(LLVM_COV_INSTALL)"; exit 1; }
endef

coverage:
	$(REQUIRE_LLVM_COV)
	$(CARGO) llvm-cov --workspace --summary-only --ignore-filename-regex 'codegraph-bench'

coverage-html:
	$(REQUIRE_LLVM_COV)
	$(CARGO) llvm-cov --workspace --html --ignore-filename-regex 'codegraph-bench'
	@echo "HTML report: $(LLVM_COV_HTML)"

coverage-lcov:
	$(REQUIRE_LLVM_COV)
	$(CARGO) llvm-cov --workspace --lcov --output-path lcov.info --ignore-filename-regex 'codegraph-bench'

coverage-open: coverage-html
	@if command -v xdg-open >/dev/null 2>&1; then xdg-open "$(LLVM_COV_HTML)" >/dev/null 2>&1 || true; \
	elif command -v open >/dev/null 2>&1; then open "$(LLVM_COV_HTML)" >/dev/null 2>&1 || true; \
	else echo "Open $(LLVM_COV_HTML) manually"; fi

coverage-clean:
	$(REQUIRE_LLVM_COV)
	$(CARGO) llvm-cov clean --workspace

hooks setup-hooks:
	git config core.hooksPath .githooks
	@echo "Enabled .githooks (pre-push runs make pre-ci)."

clean:
	$(CARGO) clean
	@rm -rf $(DIST_DIR)

size-compare: build-dev
	@ls -lh "$(TARGET_DIR)/debug/$(BINARY_NAME)"
	@if [ -f "$(TARGET_DIR)/release/$(BINARY_NAME)" ]; then ls -lh "$(TARGET_DIR)/release/$(BINARY_NAME)"; fi

help:
	@printf '%s\n' \
		'Targets:' \
		'  check / ci           complete quality gate (ci is an alias)' \
		'  pre-ci               complete gate, viewer frontend, and local archive smoke' \
		'  ui / ui-check        rebuild the viewer bundle / also check, test, and byte-check it' \
		'  fmt / fmt-check       Rust plus repository text formatting' \
		'  typecheck              locked cargo check for the workspace' \
		'  lint / test / release locked Rust gates and shipped build' \
		'  workflow-lint         actionlint plus shellcheck' \
		'  docs-check             local Markdown links, mirrors, and source contracts' \
		'  workspace-version     version/Cargo.lock consistency gate' \
		'  coverage[-html|-lcov] informational coverage reports' \
		'  archive-smoke          package, unpack, and execute local release bytes' \
		'  hooks                  enable the versioned pre-push hook'
