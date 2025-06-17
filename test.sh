#!/bin/bash

set -eE -o functrace

failure() {
  local lineno=$1
  local msg=$2
  echo "Failed at $lineno: $msg"
}
trap 'failure ${LINENO} "$BASH_COMMAND"' ERR

cli=./target/debug/firefish-cli
upgraded_cli="./target/debug/firefish-cli-new"

# This can be used to detect desync between this script and the callers in CI etc.
if [ "$1" = "--expected-upgrade-step-count" ];
then
	expected_upgrade_step_count="$2"
	shift
	shift
fi

current_step=0
if [ "$1" = "--upgrade-step" ];
then
	upgrade_step="$2"
	shift
	shift
else
	upgrade_step=-1
fi

if [ "$1" '!=' "--external" ] && [ "$1" '!=' '--bitcoin-cli' ];
then
	echo 'You must specify `--external` or `--bitcoin-cli`' >&2
	echo 'Note that `--external` is manual' >&2
	exit 1
fi

test_kind=$1

if [ "$test_kind" = "--bitcoin-cli" ];
then
	if [ -n "$2" ];
	then
		bitcoin_cli="$2"
	else
		bitcoin_cli=bitcoin-cli
	fi
	bitcoin_cli="$bitcoin_cli -chain=regtest"
fi

did_create_wallets=false

function setup_wallet() {
	if ! $bitcoin_cli listwallets | grep "$1";
	then
		if ! $bitcoin_cli loadwallet "$1";
		then
			$bitcoin_cli createwallet "$1"
			did_create_wallets=true
		fi
	fi
}

function upgrade_checkpoint() {
	if [ "$current_step" -eq "$upgrade_step" ];
	then
		cli="$upgraded_cli"
	fi
	current_step="`expr "$current_step" + 1`"
}

liquidator_fee_bump_address=bcrt1q4x43007xdalq86eushmhmkm6thqryx8y5n6lmy
borrower_return_address=bcrt1qj900stdekll8vj5juy69xpm3kqj4eyqqtrrf4x
borrower_fee_bump_address=bcrt1qazr582faujrzlnp2ztrq2y45hhn9y2dqntuffn

if [ "$test_kind" = "--bitcoin-cli" ];
then
	setup_wallet borrower
	setup_wallet liquidator_default
	setup_wallet liquidator_liquidation
	setup_wallet firefish
	if $did_create_wallets;
	then
		# it seems creating wallets is asynchronous and we have no way to block on it :(
		sleep 3
	fi
	borrower_mining_address="`$bitcoin_cli -rpcwallet=borrower getnewaddress`"
	$bitcoin_cli generatetoaddress 101 "$borrower_mining_address"
fi

# Prepares working directory, addresses etc and makes the prefund transaction.
#
# To avoid duplicating upgrade checks they are only performed if this is passed --first argument.
# This way all numbers for upgrade steps are distinct checks and don't need to be filtered-out
# (which would be annoying to maintain) or re-run.
function prepare_and_prefund() {
	cli_api_version="`$cli print api-version`"
	if [ "$test_kind" = "--bitcoin-cli" ];
	then
		liquidator_address_default="`$bitcoin_cli -rpcwallet=liquidator_default getnewaddress`"
		if [ "$cli_api_version" -gt 0 ];
		then
			liquidator_address_liquidation="`$bitcoin_cli -rpcwallet=liquidator_liquidation getnewaddress`"
			liquidation_wallet=liquidator_liquidation
		else
			liquidator_address_liquidation="$liquidator_address_default"
			liquidation_wallet=liquidator_default
		fi
		liquidator_fee_bump_address="`$bitcoin_cli -rpcwallet=firefish getnewaddress`"
		borrower_return_address="`$bitcoin_cli -rpcwallet=borrower getnewaddress`"
		borrower_fee_bump_address="`$bitcoin_cli -rpcwallet=firefish getnewaddress`"
	fi

	temp=`mktemp -d`
	echo "Test data will be saved to $temp"

	ted_o_priv_key_path="$temp/ted-o.priv"
	ted_p_priv_key_path="$temp/ted-p.priv"
	ted_o_state_path="$temp/ted-o.state"
	ted_p_state_path="$temp/ted-p.state"
	borrower_state_file="$temp/borrower.state"

	ted_o_pub="$($cli key gen ted-o $ted_o_priv_key_path)"
	ted_p_pub="$($cli key gen ted-p $ted_p_priv_key_path)"
	# TODO: gen addresses
	if [ "$cli_api_version" -gt 0 ];
	then
		offer="$($cli offer create regtest '100000 sat' "$liquidator_address_default" "$liquidator_address_liquidation" "$liquidator_fee_bump_address" "$(date --rfc-3339=seconds --date='+10 seconds' | tr ' ' 'T')" "$(date --rfc-3339=seconds --date='+2 seconds' | tr ' ' 'T')" "$ted_o_pub" "$ted_p_pub")"
	else
		offer="$($cli offer create regtest '100000 sat' "$liquidator_address_default" "$liquidator_fee_bump_address" "$(date --rfc-3339=seconds --date='+10 seconds' | tr ' ' 'T')" "$(date --rfc-3339=seconds --date='+2 seconds' | tr ' ' 'T')" "$ted_o_pub" "$ted_p_pub")"
	fi

	echo "Offer: $offer"

	echo $offer | $cli offer assign $ted_o_priv_key_path $ted_o_state_path
	echo $offer | $cli offer assign $ted_p_priv_key_path $ted_p_state_path

	output="$(echo $offer | $cli offer accept $borrower_state_file 10 "$borrower_return_address")"
	spend_info="$(echo "$output" | tail -n 1)"
	funding_address="$(echo "$output" | grep '^Funding address: ' | sed 's/^Funding address: //')"
	echo "spend info: $spend_info."
	echo "$spend_info" | $cli prefund set-spend-info $ted_o_state_path
	echo "$spend_info" | $cli prefund set-spend-info $ted_p_state_path
	if [ "$1" = "--first" ]; then upgrade_checkpoint; fi

	echo "$output"

	if [ "$test_kind" = "--bitcoin-cli" ];
	then
		btc_amt_to_send=1
		prefund_txid="`$bitcoin_cli -rpcwallet=borrower sendtoaddress $funding_address $btc_amt_to_send`"
		prefund_raw_tx="`$bitcoin_cli getrawtransaction $prefund_txid`"
	fi
}

function escrow() {
	if [ "$test_kind" = "--bitcoin-cli" ];
	then
		presigned_states="$(echo "$prefund_raw_tx" | $cli escrow init-from-prefund "$borrower_state_file" 1000 1000 "$borrower_fee_bump_address" | tail -n 1)"
	else
		presigned_states="$($cli escrow init-from-prefund "$borrower_state_file" 1000 1000 "$borrower_fee_bump_address" | tail -n 1)"
	fi

	# Check that cancel is possible even after doing init-from-prefund
	echo $prefund_raw_tx | $cli prefund cancel "$borrower_state_file" 1000

	output="$(echo $presigned_states | "$cli" escrow presign "$ted_o_state_path")"
	ted_o_sigs="$(echo "$output" | tail -n 1)"
	echo "$output"
	output="$(echo $presigned_states | $cli escrow presign "$ted_p_state_path")"
	if [ "$1" = "--first" ]; then upgrade_checkpoint; fi
	ted_p_sigs="$(echo "$output" | tail -n 1)"
	echo "$output"

	output="$(cat <<EOF | $cli escrow sign-from-prefund $borrower_state_file $ted_o_sigs $ted_p_sigs
I have backed it up
EOF
	upgrade_checkpoint
)"
	echo "$output"
	recover_tx="`echo "$output" | sed -n '/IMPORTANT: You MUST backup the following transaction!/,/^$/p' | head -n 2 | tail -n 1`"
	escrow_tx="`echo "$output" | tail -n 1`"
	if [ $test_kind = "--bitcoin-cli" ];
	then
		$bitcoin_cli sendrawtransaction "$escrow_tx" 0
	fi
	# Check that cancel is possible even after all transactions are signed
	echo $prefund_raw_tx | $cli prefund cancel "$borrower_state_file" 1000
	# As opposed to the others this one is intentionally unconditional because we need to make sure that each operation following this is checked.
	upgrade_checkpoint
}

prepare_and_prefund --first

# test cancel, which is simpler, before escrow
if [ $test_kind = "--bitcoin-cli" ];
then
	output="`echo $prefund_raw_tx | $cli prefund cancel "$borrower_state_file" 1000`"
	upgrade_checkpoint
	tx="$(echo "$output" | tail -n 1)"
	echo "$output"
	if $bitcoin_cli sendrawtransaction "$tx" 0;
	then
		"Sending transaction before prefund was mined should've failed"
		exit 1
	fi
	$bitcoin_cli generatetoaddress 42 "$borrower_mining_address"
	$bitcoin_cli sendrawtransaction "$tx" 0
	sleep 0.3
	$bitcoin_cli generatetoaddress 6 "$borrower_mining_address"
	sleep 0.3
	received="`$bitcoin_cli -rpcwallet=borrower getreceivedbyaddress "$borrower_return_address"`"
	echo "$received bitcoins returned to the borrower"
	#expr $received '=' 

	prepare_and_prefund
fi

escrow --first

# Test the remaining scenarios
if [ $test_kind = "--bitcoin-cli" ];
then
	output="`echo "$ted_o_sigs" | $cli escrow repayment $ted_p_state_path`"
	upgrade_checkpoint
	tx="$(echo "$output" | tail -n 1)"
	$bitcoin_cli sendrawtransaction "$tx" 0
	sleep 0.3
	$bitcoin_cli generatetoaddress 6 "$borrower_mining_address"
	sleep 0.3
	received="`$bitcoin_cli -rpcwallet=borrower getreceivedbyaddress "$borrower_return_address"`"
	echo "$received bitcoins returned to the borrower"
	test "$received" = 0.99617206

	prepare_and_prefund
	escrow

	output="`echo "$ted_o_sigs" | $cli escrow default $ted_p_state_path`"
	upgrade_checkpoint
	tx="$(echo "$output" | tail -n 1)"
	sleep 2
	$bitcoin_cli sendrawtransaction "$tx" 0
	sleep 0.3
	$bitcoin_cli generatetoaddress 6 "$borrower_mining_address"
	sleep 0.3
	received="`$bitcoin_cli -rpcwallet=liquidator_default getreceivedbyaddress "$liquidator_address_default"`"
	echo "The liquidator received $received bitcoins"
	test "$received" = 0.99617206

	prepare_and_prefund
	escrow

	ted_o_sig="`$cli escrow liquidation $ted_o_state_path | tail -n 1`"
	upgrade_checkpoint
	echo "$ted_o_sig"
	output="`echo "$ted_o_sig" | $cli escrow liquidation $ted_p_state_path`"
	# No upgrade_checkpoint here because this is the last call to $cli
	tx="$(echo "$output" | tail -n 1)"
	$bitcoin_cli sendrawtransaction "$tx" 0
	sleep 0.3
	$bitcoin_cli generatetoaddress 6 "$borrower_mining_address"
	sleep 0.3
	received="`$bitcoin_cli -rpcwallet="$liquidation_wallet" getreceivedbyaddress "$liquidator_address_liquidation"`"
	echo "The liquidator received $received bitcoins"
	test "$received" = 0.99617206

	prepare_and_prefund
	escrow

	# Ensure median of latest 12 blocks is at least 10 seconds later
	echo "Waiting for lock time to pass"
	sleep 11
	$bitcoin_cli generatetoaddress 12 "$borrower_mining_address"

	sleep 0.3
	$bitcoin_cli sendrawtransaction "$recover_tx" 0
	sleep 0.3
	$bitcoin_cli generatetoaddress 6 "$borrower_mining_address"
	received="`$bitcoin_cli -rpcwallet=borrower getreceivedbyaddress "$borrower_return_address"`"
	echo "$received bitcoins returned to the borrower"
	test "$received" = 0.99617206
fi

echo "Note: the script executed $current_step potential upgrade steps"

if [ -n "$expected_upgrade_step_count" ];
then
	if [ "$current_step" -ne "$expected_upgrade_step_count" ];
	then
		echo "Error: expected $expected_upgrade_step_count potential upgrade steps"
		exit 1
	fi
fi
