#!/usr/bin/env sh
set -eu

wasm_path=${1:?Usage: deploy-testnet.sh <optimized-wasm>}
network=${NETWORK:-testnet}

if [ "$network" != testnet ]; then
	printf '%s\n' 'This deployment script only supports testnet.' >&2
	exit 1
fi

: "${STELLAR_ACCOUNT:?Set STELLAR_ACCOUNT to the funded testnet account used to sign init}"
: "${ADMIN_ADDRESS:?Set ADMIN_ADDRESS}"
: "${SECONDARY_ADMIN_ADDRESS:?Set SECONDARY_ADMIN_ADDRESS}"
: "${TREASURY_ADDRESS:?Set TREASURY_ADDRESS}"
: "${DEFAULT_FEE_PCT:?Set DEFAULT_FEE_PCT (0-100)}"
: "${MAX_COURSES_PER_INSTRUCTOR:?Set MAX_COURSES_PER_INSTRUCTOR}"
: "${REFUND_WINDOW_LEDGERS:?Set REFUND_WINDOW_LEDGERS}"
: "${REVOCATION_CHALLENGE_PERIOD:?Set REVOCATION_CHALLENGE_PERIOD}"

if [ ! -f "$wasm_path" ]; then
	printf 'Optimized WASM not found: %s\n' "$wasm_path" >&2
	exit 1
fi

contract_id=$(stellar contract deploy \
	--wasm "$wasm_path" \
	--source "$STELLAR_ACCOUNT" \
	--network "$network")

printf 'Deployed testnet contract: %s\n' "$contract_id"
printf 'Admin: %s\nSecondary admin: %s\nTreasury: %s\nDefault fee: %s%%\nMax courses per instructor: %s\nRefund window (ledgers): %s\nRevocation challenge period: %s ledgers\n' \
	"$ADMIN_ADDRESS" \
	"$SECONDARY_ADMIN_ADDRESS" \
	"$TREASURY_ADDRESS" \
	"$DEFAULT_FEE_PCT" \
	"$MAX_COURSES_PER_INSTRUCTOR" \
	"$REFUND_WINDOW_LEDGERS" \
	"$REVOCATION_CHALLENGE_PERIOD"

stellar contract invoke \
	--id "$contract_id" \
	--source "$STELLAR_ACCOUNT" \
	--network "$network" \
	-- init \
	--admin "$ADMIN_ADDRESS" \
	--secondary_admin "$SECONDARY_ADMIN_ADDRESS" \
	--treasury "$TREASURY_ADDRESS" \
	--default_fee_pct "$DEFAULT_FEE_PCT" \
	--max_courses_per_instructor "$MAX_COURSES_PER_INSTRUCTOR" \
	--refund_window_ledgers "$REFUND_WINDOW_LEDGERS" \
	--revocation_challenge_period "$REVOCATION_CHALLENGE_PERIOD"

printf 'Contract initialized successfully.\n'