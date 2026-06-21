.PHONY: all build build-dev build-prod release release-target install uninstall \
        fmt fmt-rust fmt-oxfmt fmt-check fmt-rust-check fmt-oxfmt-check \
        lint check test guardrail ci hooks setup-hooks clean size-compare help

# Project configuration
PROJECT_NAME := codegraph-rs
BINARY_NAME  := codegraph
CLI_CRATE    := codegraph-rs
CARGO        := cargo

# Directories
TARGET_DIR := target
DIST_DIR   := dist

# Cross-compilation target (set via environment variable or command line)
# Example: make release-target TARGET=x86_64-unknown-linux-musl
TARGET ?=

# Default target
all: build

# Build the CLI in debug mode and copy the binary into dist/
build: build-dev

build-dev:
	@echo "🔨 Building $(PROJECT_NAME) (debug)..."
	$(CARGO) build -p $(CLI_CRATE)
	@mkdir -p $(DIST_DIR)
	@cp $(TARGET_DIR)/debug/$(BINARY_NAME) $(DIST_DIR)/$(BINARY_NAME) 2>/dev/null || \
		echo "⚠️  Binary not found, check Cargo.toml [[bin]] configuration"
	@echo "✅ Debug build complete: $(DIST_DIR)/$(BINARY_NAME)"

build-prod: release

# Build the optimized release binary and copy it into dist/
release:
	@echo "🚀 Building $(PROJECT_NAME) (release)..."
	$(CARGO) build --release -p $(CLI_CRATE)
	@mkdir -p $(DIST_DIR)
	@cp $(TARGET_DIR)/release/$(BINARY_NAME) $(DIST_DIR)/$(BINARY_NAME) 2>/dev/null || \
		echo "⚠️  Binary not found, check Cargo.toml [[bin]] configuration"
	@echo "✅ Release build complete: $(DIST_DIR)/$(BINARY_NAME)"
	@ls -lh $(DIST_DIR)/$(BINARY_NAME) 2>/dev/null || true

# Build the release binary for a specific target (cross-compilation).
# Usage: make release-target TARGET=x86_64-unknown-linux-musl
release-target:
ifndef TARGET
	$(error TARGET is not set. Usage: make release-target TARGET=x86_64-unknown-linux-musl)
endif
	@echo "🚀 Building $(PROJECT_NAME) for target $(TARGET)..."
	$(CARGO) build --release -p $(CLI_CRATE) --target $(TARGET)
	@mkdir -p $(DIST_DIR)
	@if [ -f "$(TARGET_DIR)/$(TARGET)/release/$(BINARY_NAME)" ]; then \
		cp $(TARGET_DIR)/$(TARGET)/release/$(BINARY_NAME) $(DIST_DIR)/$(BINARY_NAME)-$(TARGET); \
	elif [ -f "$(TARGET_DIR)/$(TARGET)/release/$(BINARY_NAME).exe" ]; then \
		cp $(TARGET_DIR)/$(TARGET)/release/$(BINARY_NAME).exe $(DIST_DIR)/$(BINARY_NAME)-$(TARGET).exe; \
	else \
		echo "⚠️  Binary not found for target $(TARGET)"; \
		exit 1; \
	fi
	@echo "✅ Release build complete for $(TARGET)"
	@ls -lh $(DIST_DIR)/$(BINARY_NAME)-$(TARGET)* 2>/dev/null || true

# Install the binary to ~/.cargo/bin
install: release
	@echo "📦 Installing $(BINARY_NAME) to ~/.cargo/bin..."
	$(CARGO) install --path crates/codegraph-cli
	@echo "✅ Installation complete"

uninstall:
	@echo "🗑️  Uninstalling $(CLI_CRATE)..."
	$(CARGO) uninstall $(CLI_CRATE) || true

# oxfmt: formats Markdown / JSON / YAML etc. (Rust is owned by `cargo fmt`).
# Auto-generated files (CHANGELOG.md, release-please manifests) + golden
# fixtures are excluded via .oxfmtignore.
OXFMT := oxfmt
OXFMT_ARGS := --no-error-on-unmatched-pattern --ignore-path .oxfmtignore .

# Format code (Rust via cargo fmt + docs/markdown via oxfmt)
fmt: fmt-rust fmt-oxfmt
	@echo "✨ Formatting complete."

fmt-rust:
	@echo "✨ Formatting Rust (cargo fmt)..."
	$(CARGO) fmt --all

fmt-oxfmt:
	@echo "✨ Formatting docs (oxfmt)..."
	@if command -v $(OXFMT) >/dev/null 2>&1; then \
		$(OXFMT) $(OXFMT_ARGS); \
	else \
		echo "⚠️  oxfmt not found — skipping doc formatting. Install: npm i -g oxfmt"; \
	fi

# Check code formatting (CI: Rust + docs)
fmt-check: fmt-rust-check fmt-oxfmt-check
	@echo "✨ Format check complete."

fmt-rust-check:
	@echo "✨ Checking Rust formatting..."
	$(CARGO) fmt --all --check

fmt-oxfmt-check:
	@echo "✨ Checking doc formatting..."
	@if command -v $(OXFMT) >/dev/null 2>&1; then \
		$(OXFMT) --check $(OXFMT_ARGS); \
	else \
		echo "⚠️  oxfmt not found — skipping doc format check. Install: npm i -g oxfmt"; \
	fi

# Run linter (clippy, warnings denied)
lint:
	@echo "🔍 Running clippy..."
	$(CARGO) clippy --workspace --all-targets -- -D warnings

# Type-check the workspace without producing artifacts
check:
	@echo "✅ Checking workspace..."
	$(CARGO) check --workspace

# Run the full test suite (incl. golden oracle + equivalence)
test:
	@echo "🧪 Running tests..."
	$(CARGO) test --workspace

# Scope guardrail: no AI / vector / LLM crates allowed in the workspace
guardrail:
	@echo "🛡️  Running scope guardrail..."
	bash scripts/guardrail.sh

# Run every gate that CI enforces
ci: fmt-check lint test guardrail
	@echo "✅ All CI checks passed!"

# Enable the version-controlled pre-push hook (run once per clone).
# Points core.hooksPath at .githooks so `git push` runs the local quality gate
# (fmt + clippy + test + guardrail) before anything reaches GitHub.
hooks setup-hooks:
	@echo "🪝  Enabling version-controlled git hooks (core.hooksPath -> .githooks)..."
	git config core.hooksPath .githooks
	@echo "✅ Done. The pre-push gate is now active (fmt + clippy + test + guardrail on push)."

# Clean build artifacts
clean:
	@echo "🧹 Cleaning build artifacts..."
	$(CARGO) clean
	@rm -rf $(DIST_DIR)
	@echo "✅ Clean complete"

# Show debug vs release binary size
size-compare: build-dev
	@echo ""
	@echo "📊 Binary size comparison:"
	@echo "Debug:"
	@ls -lh $(TARGET_DIR)/debug/$(BINARY_NAME) 2>/dev/null || echo "  Not found"
	@if [ -f "$(TARGET_DIR)/release/$(BINARY_NAME)" ]; then \
		echo "Release:"; \
		ls -lh $(TARGET_DIR)/release/$(BINARY_NAME); \
	fi

help:
	@echo "Available targets:"
	@echo ""
	@echo "  Build:"
	@echo "    build        - Build the CLI (debug) into dist/"
	@echo "    release      - Build the optimized CLI (release) into dist/"
	@echo "    build-prod   - Alias for release"
	@echo "    release-target TARGET=<triple> - Cross-compile for a target"
	@echo "                   e.g. make release-target TARGET=x86_64-unknown-linux-musl"
	@echo ""
	@echo "  Install:"
	@echo "    install      - cargo install the CLI to ~/.cargo/bin"
	@echo "    uninstall    - cargo uninstall the CLI"
	@echo ""
	@echo "  Development:"
	@echo "    fmt          - Format code (cargo fmt --all)"
	@echo "    fmt-check    - Check formatting (CI)"
	@echo "    lint         - Run clippy with -D warnings"
	@echo "    check        - cargo check the workspace"
	@echo "    test         - Run the full test suite"
	@echo "    guardrail    - Run the scope guardrail (no AI/vector/LLM crates)"
	@echo "    ci           - fmt-check + lint + test + guardrail"
	@echo "    hooks        - Enable the pre-push git hook (run once after clone)"
	@echo ""
	@echo "  Utilities:"
	@echo "    clean        - Remove build artifacts and dist/"
	@echo "    size-compare - Show debug vs release binary size"
	@echo "    help         - Show this help"
	@echo ""
	@echo "  Release profile (configured in Cargo.toml [profile.release]):"
	@echo "    opt-level = 3, lto = \"fat\", codegen-units = 1, strip = true"
