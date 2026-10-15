SHELL := /bin/sh

CARGO ?= cargo
PYTHON ?= python3
INSTALL ?= install
PREFIX ?= /usr/local
USER_PREFIX ?= $(HOME)/.local
DESTDIR ?=
WHEEL_ROOT ?= build/wheel-root/aitex
WHEEL_DATA ?= build/wheel-data
DIST_DIR ?= dist
WHEEL_TARGET ?= build/wheel-target
MATURIN ?= maturin
VERSION := $(shell sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -1)
HOST_TRIPLE := $(shell rustc -vV 2>/dev/null | sed -n 's/^host: //p')
RELEASE_BIN := target/release/aitex
FORMAT := .aitex/format/fmt.afmt
NO_AI_RELEASE_BIN := target/no-ai/release/aitex
BUNDLE_BIN := bin/aitex

RUST_SOURCES := $(shell find crates -type f \( -name '*.rs' -o -name 'Cargo.toml' \))
BUILD_INPUTS := Cargo.toml Cargo.lock rust-toolchain.toml $(RUST_SOURCES)
# build.rs embeds `git describe`. Include the current ref and tag namespace in
# Make's own freshness graph so it does not prevent Cargo from observing a
# Git-only change. `wildcard` keeps exported source archives (with no `.git`)
# working normally.
GIT_HEAD_PATH := $(shell git rev-parse --git-path HEAD 2>/dev/null)
GIT_BRANCH_PATH := $(shell ref=$$(git symbolic-ref -q HEAD 2>/dev/null) && git rev-parse --git-path "$$ref" 2>/dev/null)
GIT_TAGS_PATH := $(shell git rev-parse --git-path refs/tags 2>/dev/null)
GIT_PACKED_REFS_PATH := $(shell git rev-parse --git-path packed-refs 2>/dev/null)
GIT_STATE_INPUTS := $(wildcard $(GIT_HEAD_PATH) $(GIT_BRANCH_PATH) $(GIT_TAGS_PATH) $(GIT_PACKED_REFS_PATH))
BUILD_INPUTS += $(GIT_STATE_INPUTS)

.DEFAULT_GOAL := bundle
.DELETE_ON_ERROR:

.PHONY: all build format bundle no-ai repair-lab repair-check sbom dist dist-bundle dist-source install install-user wheel-data wheel wheel-layout check clean-bundle help

all: bundle

build: $(RELEASE_BIN)

format: $(RELEASE_BIN)
	"$(abspath $(RELEASE_BIN))" format --output "$(abspath $(FORMAT))"

bundle: $(RELEASE_BIN) format
	$(INSTALL) -d "$(dir $(BUNDLE_BIN))"
	$(INSTALL) -m 0755 "$(RELEASE_BIN)" "$(BUNDLE_BIN)"
	@echo "aiTeX bundle ready: $(BUNDLE_BIN)"

# Build the same local bundle without the repair harness or provider stack.
# Separate Cargo and format paths make switching between `make` and
# `make no-ai` unambiguous even when every source file is already up to date.
no-ai: $(NO_AI_RELEASE_BIN)
	"$(abspath $(NO_AI_RELEASE_BIN))" format --output "$(abspath $(FORMAT))"
	$(INSTALL) -d "$(dir $(BUNDLE_BIN))"
	$(INSTALL) -m 0755 "$(NO_AI_RELEASE_BIN)" "$(BUNDLE_BIN)"
	@echo "aiTeX no-AI bundle ready: $(BUNDLE_BIN)"

# Development evaluator used by tools/model_campaign.py; not installed.
repair-lab:
	$(CARGO) build --locked -p aitex-harness --bin aitex-repair-lab

# Offline product-boundary check for the retained small repair lab and campaign
# accounting. Large-corpus benchmark acquisition and evaluation live outside
# this compiler repository.
repair-check:
	PYTHONDONTWRITEBYTECODE=1 $(PYTHON) tools/model_campaign.py --self-test

dist: dist-bundle dist-source

sbom:
	PYTHONDONTWRITEBYTECODE=1 $(PYTHON) tools/sbom.py --output "$(DIST_DIR)/aitex-$(VERSION).spdx.json"

dist-bundle: $(RELEASE_BIN) sbom
	PYTHONDONTWRITEBYTECODE=1 $(PYTHON) tools/package_release.py bundle --output-dir "$(DIST_DIR)"
	PYTHONDONTWRITEBYTECODE=1 $(PYTHON) tools/bundle_check.py "$(DIST_DIR)/aitex-$(VERSION)-$(HOST_TRIPLE).tar.gz"

dist-source:
	PYTHONDONTWRITEBYTECODE=1 $(PYTHON) tools/package_release.py source --output-dir "$(DIST_DIR)"

$(RELEASE_BIN): $(BUILD_INPUTS)
	$(CARGO) build --release --locked -p aitex-cli

$(NO_AI_RELEASE_BIN): $(BUILD_INPUTS)
	$(CARGO) build --release --locked -p aitex-cli --no-default-features --target-dir target/no-ai

# A staged installation follows the usual DESTDIR/PREFIX convention.
install: $(RELEASE_BIN)
	$(INSTALL) -d "$(DESTDIR)$(PREFIX)/bin"
	$(INSTALL) -m 0755 "$(RELEASE_BIN)" "$(DESTDIR)$(PREFIX)/bin/aitex"

# Unprivileged installation. Most Linux distributions already place this bin
# directory on PATH; macOS users may need to add it once in their shell setup.
install-user: PREFIX := $(USER_PREFIX)
install-user: install
	@echo "aiTeX installed under $(USER_PREFIX)"
	@case ":$$PATH:" in *":$(USER_PREFIX)/bin:"*) ;; *) \
		echo "add $(USER_PREFIX)/bin to PATH to run aitex from any directory" ;; esac

# Point WHEEL_ROOT at the Python package directory that will be put in a wheel.
wheel-layout: bundle
	$(INSTALL) -d "$(WHEEL_ROOT)/bin"
	$(INSTALL) -m 0755 "$(BUNDLE_BIN)" "$(WHEEL_ROOT)/bin/aitex"
	@echo "wheel payload ready: $(WHEEL_ROOT)"

# Maturin supplies <venv>/bin/aitex. Documentation remains wheel data; the
# user's first invocation creates the local TeX-matched format cache.
wheel-data:
	$(RM) "$(WHEEL_DATA)/data/share/aitex/latex.afmt" \
		"$(WHEEL_DATA)/data/share/doc/aitex/LPPL-1.3c.tex" \
		"$(WHEEL_DATA)/data/share/doc/aitex/LICENSE.TL"
	$(INSTALL) -d "$(WHEEL_DATA)/data/share/doc/aitex"
	$(INSTALL) -m 0644 THIRD_PARTY_NOTICES.md "$(WHEEL_DATA)/data/share/doc/aitex/THIRD_PARTY_NOTICES.md"

wheel: wheel-data
	$(MATURIN) build --release --locked --target-dir "$(WHEEL_TARGET)" --out "$(DIST_DIR)"
	$(PYTHON) tools/wheel_check.py "$(DIST_DIR)"/*.whl

check:
	$(CARGO) fmt --all --check
	$(CARGO) test --workspace --locked

clean-bundle:
	rm -f "$(BUNDLE_BIN)"
	rmdir bin share/aitex share 2>/dev/null || true

help:
	@echo "make                 build AI-enabled bin/aitex and prewarm the local format"
	@echo "make no-ai           build bin/aitex without the repair/provider stack"
	@echo "make repair-lab      build the controlled model-evaluation binary"
	@echo "make repair-check    check the local repair campaign protocol offline"
	@echo "make sbom            generate and validate the SPDX 2.3 dependency inventory"
	@echo "make dist            create and verify this host's native bundle plus source archive"
	@echo "make dist-bundle     create and verify this host's native bundle"
	@echo "make dist-source     create a source archive from a clean HEAD"
	@echo "make install         install under PREFIX=/usr/local (normally run with sudo)"
	@echo "make install-user    install without root under USER_PREFIX=$(USER_PREFIX)"
	@echo "make wheel           build and verify this platform's native wheel"
	@echo "make wheel-layout    stage bin/ + share/ under WHEEL_ROOT"
	@echo "make check           format-check and test the workspace"
