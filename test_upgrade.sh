#!/bin/bash

set -eE -o functrace

failure() {
  local lineno=$1
  local msg=$2
  echo "Failed at $lineno: $msg"
}
trap 'failure ${LINENO} "$BASH_COMMAND"' ERR

upgrade_step_count=10

# This can be used to detect desync between this script and the callers in CI etc.
if [ "$1" = "--expected-upgrade-step-count" ];
then
	if [ "$2" -ne "$upgrade_step_count" ];
	then
		echo "Error: unexpected number of upgrade steps, the caller thinks it's $2 but the script thinks it's $upgrade_step_count"
	fi
	shift
	shift
fi

if [ "$1" = "--from" ];
then
	from="$2"
	shift
	shift
else
	from="HEAD^"
fi

if [ "$1" = "--bitcoin-cli" ];
then
	bitcoin_cli="$2"
	shift
	shift
else
	bitcoin_cli="bitcoin-cli"
fi

if [ "-n" "$1" ];
then
	steps="$1"
	for step in $steps;
	do
		if [ "$step" -ge "$upgrade_step_count" ] || [ "$step" -lt 0 ];
		then
			echo "Error: Upgrade step $1 out of range" >&2
			exit 1
		fi
	done
else
	steps="$(seq 1 $(expr "$upgrade_step_count" - 1))"
fi

git_status="`git status --porcelain`"
if [ -n "$git_status" ];
then
	echo "Error: git not clean, commit the changes before running!" >&2
	exit 1
fi

cargo build -p firefish-cli

mv target/debug/firefish-cli target/debug/firefish-cli-new

git checkout "$from"

if cargo build -p firefish-cli;
then
	git checkout -
else
	git checkout -
	exit 1
fi

for step in $steps;
do
	./test.sh --expected-upgrade-step-count $upgrade_step_count --upgrade-step "$step" --bitcoin-cli "$bitcoin_cli"
done
