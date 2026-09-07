.PHONY: help build release run check test fmt fmt-check clippy clean install uninstall dev watch audit tree ci ci-fast ci-lint stability release-check docs-index cleanup-branches compiler-cache-status compiler-cache-setup compiler-cache-bench

# ------------------------------------------------------------
# Config
# ------------------------------------------------------------
CARGO ?= cargo
BINARY := orbit
BIN_CRATE := orbit-cli
# Crate sources live under orbit/ (see root Cargo.toml workspace members).
BIN_CRATE_PATH := crates/$(BIN_CRATE)
WORKSPACE := --workspace
INSTALL_PROFILE ?= release
INSTALL_BIN_DIR ?= $(HOME)/.cargo/bin

# Detect profile
PROFILE ?= debug
ifeq ($(PROFILE),release)
	CARGO_PROFILE := --release
	TARGET_DIR := target/release
else
	CARGO_PROFILE :=
	TARGET_DIR := target/debug
endif

ifeq ($(INSTALL_PROFILE),release)
	INSTALL_CARGO_PROFILE := --release
	INSTALL_TARGET_DIR := target/release
else
	INSTALL_CARGO_PROFILE :=
	INSTALL_TARGET_DIR := target/debug
endif

# ------------------------------------------------------------
# Help
# ------------------------------------------------------------
help:
	@echo "Orbit Workspace Make Targets"
	@echo ""
	@echo "  make build        Build workspace (PROFILE=release optional)"
	@echo "  make release      Build optimized release binary"
	@echo "  make run ARGS=... Run CLI binary"
	@echo "  make dev ARGS=... Run debug binary directly"
	@echo "  make check        Type-check entire workspace"
	@echo "  make test         Run all tests"
	@echo "  make fmt          Format code"
	@echo "  make fmt-check    Check formatting"
	@echo "  make clippy       Lint with clippy (deny warnings)"
	@echo "  make audit        Supply-chain audit (cargo-deny: advisories + licenses)"
	@echo "  make tree         Print dependency tree"
	@echo "  make ci           Full CI pass (clippy + tests + doc + guardrails; also runs on PRs)"
	@echo "  make ci-fast      Pre-handoff gate for agents (fmt-check + guardrail scripts; no compile)"
	@echo "  make ci-lint      Pre-handoff clippy gate for agents (compiles all workspace targets)"
	@echo "  make docs-index   Regenerate docs/INDEX.md"
	@echo "  make stability    Verify per-crate stability tier markers"
	@echo "  make release-check  Verify Cargo/npm/release version lockstep (see docs/runbooks/release.md)"
	@echo "  make install      Install CLI locally (INSTALL_PROFILE=debug optional)"
	@echo "  make uninstall    Remove installed binary"
	@echo "  make clean        Clean build artifacts"
	@echo "  make cleanup-branches  Force-remove worktrees and branches except main/agent-main (DESTRUCTIVE)"
	@echo "  make compiler-cache-status  Show whether the opt-in rustc cache would enable"
	@echo "  make compiler-cache-setup   Create ~/.orbit/cache/compiler (SETUP_FLAGS=--install to fetch sccache)"
	@echo "  make compiler-cache-bench   Two-worktree cold/warm/concurrent compiler-cache timings"
	@echo "  make watch        Continuous check + test"

# ------------------------------------------------------------
# Build
# ------------------------------------------------------------
build:
	$(CARGO) build $(WORKSPACE) $(CARGO_PROFILE)

release:
	$(CARGO) build -p $(BIN_CRATE) --bin $(BINARY) --release

# ------------------------------------------------------------
# Run
# ------------------------------------------------------------
run:
	$(CARGO) run -p $(BIN_CRATE) --bin $(BINARY) -- $(ARGS)

# Direct execution (after build)
dev: build
	$(TARGET_DIR)/$(BINARY) $(ARGS)

# ------------------------------------------------------------
# Quality
# ------------------------------------------------------------
check:
	$(CARGO) check $(WORKSPACE)

test:
	$(CARGO) test $(WORKSPACE)

fmt:
	$(CARGO) fmt --all

fmt-check:
	$(CARGO) fmt --all -- --check

clippy:
	$(CARGO) clippy $(WORKSPACE) --all-targets -- -D warnings

# Supply-chain audit: advisories + license allow-list via cargo-deny (deny.toml).
# Canonical command; CI runs the same check via scripts/ci-guardrails.sh.
audit:
	@command -v cargo-deny >/dev/null 2>&1 || { echo "Install cargo-deny via: cargo install cargo-deny --locked"; exit 1; }
	$(CARGO) deny check

# Dependency tree inspection
tree:
	$(CARGO) tree -e features

# Full CI pass
ci:
	./scripts/ci-guardrails.sh

# Pre-handoff gate for agents: fast checks, no compile. Full make ci runs on PRs.
ci-fast:
	cargo fmt --all -- --check
	./scripts/generate-doc-indexes.sh --check
	./scripts/check-installer-pubkey.sh
	./scripts/test-installer-security.sh
	./scripts/check-dependency-direction.sh
	./scripts/check-cli-imports.sh
	./scripts/check-terminal-state-guard.sh
	./scripts/check-history-note-size.sh
	./scripts/check-stability.sh
	./scripts/check-artifact-redaction-guardrail.sh
	./scripts/check-public-artifact-ids.py
	./scripts/check-changelog-style.sh
	./scripts/check-error-translation.sh
	./scripts/check-orphan-modules.sh
	./scripts/check-crate-agent-guides.sh
	./scripts/check-embedded-asset-portability.py
	./scripts/sync-activity-assets.sh --check
	./scripts/sync-plugin-skills.sh --check
	./scripts/test-validate-codex-plugin.sh
	./scripts/test-validate-agent-plugin.sh
	./scripts/test-cursor-marketplace-followup.sh
	./scripts/smoke-plugin-install.sh
	./scripts/test-compiler-cache.sh

# Compile-time pre-handoff gate for agents. Keep this invocation aligned with
# the default workspace clippy pass in scripts/ci-guardrails.sh.
ci-lint:
	$(CARGO) clippy $(WORKSPACE) --all-targets -- -D warnings

# Verify every workspace crate declares its stability tier
stability:
	./scripts/check-stability.sh

# Verify Cargo/npm/plugin/release version invariant before cutting a release
release-check:
	./scripts/release-check.sh

# Regenerate human-authored documentation indexes from source frontmatter.
docs-index:
	./scripts/generate-doc-indexes.sh

# ------------------------------------------------------------
# Install
# ------------------------------------------------------------
install:
	$(CARGO) build -p $(BIN_CRATE) $(INSTALL_CARGO_PROFILE)
	install -d $(INSTALL_BIN_DIR)
	install -m 755 $(INSTALL_TARGET_DIR)/$(BINARY) $(INSTALL_BIN_DIR)/$(BINARY)

uninstall:
	rm -f $(INSTALL_BIN_DIR)/$(BINARY)

# ------------------------------------------------------------
# Clean
# ------------------------------------------------------------
clean:
	$(CARGO) clean

# Force-remove every linked worktree and local branch except main/agent-main.
# Destructive: discards in-progress task branches and uncommitted work in worktrees.
cleanup-branches:
	./scripts/cleanup-branches.sh

# Opt-in host compiler cache shared across worktrees (sccache). Falls back to
# ordinary rustc when the cache is missing or unwritable. See
# docs/runbooks/compiler-cache.md. [ORB-11259]
compiler-cache-status:
	./scripts/compiler-cache.sh status

compiler-cache-setup:
	./scripts/compiler-cache.sh setup $(SETUP_FLAGS)

compiler-cache-bench:
	./scripts/bench-compiler-cache.sh

# ------------------------------------------------------------
# Dev Loop
# ------------------------------------------------------------
watch:
	$(CARGO) watch -x "check" -x "test"
