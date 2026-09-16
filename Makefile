# bmkg-edge — everything you need, and `clean` that actually reclaims the space.
SHELL := /bin/bash
export PATH := $(HOME)/.cargo/bin:$(PATH)

D1 := bmkg-wilayah

.PHONY: help build check test fmt lint deploy seed seed-local dev size clean distclean

help:
	@grep -E '^[a-z-]+:.*?## .*$$' $(MAKEFILE_LIST) | sed 's/:.*## /\t/' | column -t -s $$'\t'

build: ## Compile the Worker to WASM (release)
	worker-build --release

check: ## Type-check for the Worker target
	cargo check --target wasm32-unknown-unknown

test: ## Run unit tests on the host
	cargo test

fmt: ## Format
	cargo fmt

lint: ## Clippy, warnings as errors
	cargo clippy --target wasm32-unknown-unknown -- -D warnings

wilayah: ## Rebuild the region seed from the upstream dataset
	python3 tools/build-wilayah.py

seed: wilayah ## Load the region seed into the remote D1 database
	wrangler d1 execute $(D1) --remote --yes --file=data/wilayah.seed.sql

seed-local: wilayah ## Load the region seed into the local D1 database
	wrangler d1 execute $(D1) --local --yes --file=data/wilayah.seed.sql

deploy: ## Deploy to Cloudflare
	wrangler deploy

dev: ## Run locally (not a deliverable — always verify on the deployed URL)
	wrangler dev

size: ## Report the WASM bundle against the budget in GRANDPLAN §9
	@test -d build || { echo "run 'make build' first"; exit 1; }
	@find build -name '*.wasm' -printf '%f  %s bytes\n'
	@find build -name '*.wasm' -exec gzip -c {} \; | wc -c | awk '{printf "gzipped      %d bytes  (budget 3145728)\n", $$1}'

clean: ## Reclaim build artefacts — this is the one that frees gigabytes
	cargo clean
	rm -rf $(CURDIR)/build
	rm -rf $(CURDIR)/.wrangler
	@du -sh $(CURDIR)

distclean: clean ## Also drop the generated region seed
	rm -f $(CURDIR)/data/wilayah.seed.sql
	rm -f $(CURDIR)/data/wilayah.source.sql
	@du -sh $(CURDIR)
