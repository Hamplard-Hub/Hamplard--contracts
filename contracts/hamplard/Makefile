# Hamplard Contract — Makefile

NETWORK       = testnet
CONTRACT_NAME = hamplard
WASM_PATH     = target/wasm32v1-none/release/$(CONTRACT_NAME).wasm
WASM_OPT      = target/wasm32v1-none/release/$(CONTRACT_NAME).optimized.wasm

.PHONY: test build optimize deploy-testnet deploy-and-init-testnet clean fmt fmt-check check lint snapshots

test:
	cargo test

build:
	stellar contract build

optimize: build
	stellar contract optimize --wasm $(WASM_PATH)
	@echo "Optimized: $(WASM_OPT)"

deploy-testnet:
	@: "$${STELLAR_ACCOUNT:?Set STELLAR_ACCOUNT to the funded testnet account used to deploy and sign __constructor}" \
		"$${ADMIN_ADDRESS:?Set ADMIN_ADDRESS}" \
		"$${SECONDARY_ADMIN_ADDRESS:?Set SECONDARY_ADMIN_ADDRESS}" \
		"$${TREASURY_ADDRESS:?Set TREASURY_ADDRESS}" \
		"$${DEFAULT_FEE_PCT:?Set DEFAULT_FEE_PCT (0-100)}" \
		"$${MAX_COURSES_PER_INSTRUCTOR:?Set MAX_COURSES_PER_INSTRUCTOR}" \
		"$${REFUND_WINDOW_LEDGERS:?Set REFUND_WINDOW_LEDGERS}" \
		"$${REVOCATION_CHALLENGE_PERIOD:?Set REVOCATION_CHALLENGE_PERIOD}"
	$(MAKE) optimize
	NETWORK="$(NETWORK)" sh ./scripts/deploy-testnet.sh "$(WASM_OPT)"

deploy-and-init-testnet: deploy-testnet

fmt:
	cargo fmt

fmt-check:
	cargo fmt --check

check:
	cargo check

lint:
	cargo clippy --all-targets -- -D warnings

snapshots:
	cargo clean
	cargo test

clean:
	cargo clean
