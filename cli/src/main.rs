use std::io::{Read, Write};
use firefish_core::contract;
use core::convert::TryInto;
use contract::participant::{self, Ted};
use contract::{Serialize, Deserialize, prefund, escrow};
use bitcoin::key::Keypair;
use bitcoin::TxOut;
use secp256k1::SECP256K1;

fn offer_create(mut args: std::env::ArgsOs) {
    use contract::offer::AnyTedSigKeys::*;

    let network = args
        .next()
        .expect("missing bitcoin network")
        .into_string()
        .expect("bitcoin network is not UTF-8")
        .parse::<bitcoin::Network>()
        .expect("invalid bitcoin network");
    let liquidator_amount = args.next()
        .expect("missing liquidator amount")
        .into_string()
        .expect("liquidator amount is not UTF-8")
        .parse::<bitcoin::Amount>()
        .expect("failed to parse liquidator amount");
    let liquidator_address_default = args.next()
        .expect("missing liquidator address for default")
        .into_string()
        .expect("liquidator address is not UTF-8")
        .parse::<bitcoin::Address<_>>()
        .expect("invalid bitcoin address");
    let liquidator_address_liquidation = args.next()
        .expect("missing liquidator address for liquidation")
        .into_string()
        .expect("liquidator address is not UTF-8")
        .parse::<bitcoin::Address<_>>()
        .expect("invalid bitcoin address");


    let liquidator_address_default = liquidator_address_default.require_network(network)
        .expect("The liquidator address belongs to a different network");
    let liquidator_address_liquidation = liquidator_address_liquidation.require_network(network)
        .expect("The liquidator address belongs to a different network");

    let fee_bump_address = args.next()
        .expect("missing fee bump address")
        .into_string()
        .expect("fee bump address is not UTF-8")
        .parse::<bitcoin::Address<_>>()
        .expect("invalid bitcoin address")
        .require_network(network)
        .expect("The fee bump address belongs to a different network");

    let fee_bump_output = TxOut::minimal_non_dust(fee_bump_address.script_pubkey());

    let current_unix_time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("misconfigured system time (before existence of Bitcoin)")
        .as_secs();
    let recover_lock_time = args.next()
        .expect("missing time lock")
        .into_string()
        .expect("time lock is not UTF-8");
    let recover_lock_time = chrono::DateTime::parse_from_rfc3339(&recover_lock_time)
        .expect("failed to parse time lock - the format has to be RFC 3339")
        .timestamp();
    // Sanity check
    assert!(current_unix_time >= 1_231_006_505, "misconfigured system time (before Bitcoin genesis block)");
    let recover_lock_time: u64 = recover_lock_time.try_into().expect("time lock is in the past");
    assert!(recover_lock_time >= current_unix_time, "time lock is in the past");
    let recover_lock_time: u32 = recover_lock_time.try_into().expect("time lock is past the Bitcoin overflow bug");
    // The current unix time is above genesis block and genesis block is above lock time threshold
    let recover_lock_time = bitcoin::absolute::LockTime::from_time(recover_lock_time).expect("if you can see this there's a bug in the program");
    let default_lock_time = args.next()
        .expect("missing time lock")
        .into_string()
        .expect("time lock is not UTF-8");
    let default_lock_time = chrono::DateTime::parse_from_rfc3339(&default_lock_time)
        .expect("failed to parse time lock - the format has to be RFC 3339")
        .timestamp();
    // Sanity check
    assert!(current_unix_time >= 1_231_006_505, "misconfigured system time (before Bitcoin genesis block)");
    let default_lock_time: u64 = default_lock_time.try_into().expect("time lock is in the past");
    assert!(default_lock_time >= current_unix_time, "time lock is in the past");
    let default_lock_time: u32 = default_lock_time.try_into().expect("time lock is past the Bitcoin overflow bug");
    // The current unix time is above genesis block and genesis block is above lock time threshold
    let default_lock_time = bitcoin::absolute::LockTime::from_time(default_lock_time).expect("if you can see this there's a bug in the program");
    assert!(default_lock_time < recover_lock_time);

    let mut ted_o = None;
    let mut ted_p = None;

    for keys in args.by_ref().take(2) {
        let keys = keys
            .into_string()
            .expect("key is not an UTF-8 string")
            .parse::<contract::offer::AnyTedSigKeys>()
            .expect("failed to parse TedSig keys");
        match (keys, &ted_o, &ted_p) {
            (TedO(keys), None, _) => ted_o = Some(keys),
            (TedO(_), Some(_), _) => panic!("TED-O keys entered twice"),
            (TedP(keys), _, None) => ted_p = Some(keys),
            (TedP(_), _, Some(_)) => panic!("TED-P keys entered twice"),
        }
    }

    let (ted_o_keys, ted_p_keys) = match (ted_o, ted_p) {
        (Some(ted_o), Some(ted_p)) => (ted_o, ted_p),
        (None, Some(_)) => panic!("missing TED-O public keys"),
        (Some(_), None) => panic!("missing TED-P public keys"),
        (None, None) => panic!("missing TedSig public keys"),
    };

    let mut optional_fields = contract::offer::OptionalOfferFields::default();
    optional_fields.extra_termination_outputs.push(fee_bump_output);
    let offer = contract::offer::MandatoryOfferFields {
        network,
        liquidator_script_default: liquidator_address_default.script_pubkey(),
        liquidator_script_liquidation: liquidator_address_liquidation.script_pubkey(),
        min_collateral: liquidator_amount,
        recover_lock_time,
        default_lock_time,
        ted_o_keys,
        ted_p_keys,
    }.into_offer_with_optional(optional_fields);
    let mut buf = Vec::new();
    offer.serialize(&mut buf);

    match args.next() {
        Some(path) => write_non_existing(&path, &buf),
        None => {
            let encoded = base64::encode(buf);
            println!("{}", encoded);
        },
    }
}

fn offer_decode(mut args: std::env::ArgsOs) {
    let offer = load_offer(&mut args);
    println!("{:#?}", offer);
}

fn offer_accept(mut args: std::env::ArgsOs) {
    let state_path = args.next().expect("missing state file path");
    let lock_time = args.next().expect("missing sequence number (relative lock time)");
    let return_address = args.next().expect("missing return address");
    let offer = load_offer(&mut args);

    let lock_time = lock_time.into_string()
        .expect("lock time is not UTF-8")
        .parse()
        .expect("invalid sequence number");
    let return_address = return_address.into_string()
        .expect("lock time is not UTF-8")
        .parse::<bitcoin::Address<_>>()
        .expect("invalid bitcoin address")
        .require_network(offer.escrow.network)
        .expect("The return address belongs to a different network");

    let key_pair = Keypair::new(SECP256K1, &mut secp256k1::rand::thread_rng());

    let params = participant::borrower::MandatoryPrefundParams {
        key_pair,
        lock_time,
        return_script: return_address.script_pubkey(),
    };

    let borrower = participant::borrower::init_prefund(offer, params.into_params());
    let mut state = Vec::new();
    borrower.serialize(&mut state);
    let mut message = Vec::new();
    borrower.borrower_info().serialize(&mut message);
    let message = base64::encode(message);
    write_non_existing(&state_path, &state);

    println!();
    println!("=========================================================");
    println!("!!! WARNING !!!");
    println!("You MUST bakup the state file before sending any satoshis!");
    println!("The state file is NOT encrypted!");
    println!("=========================================================");
    println!();
    println!("Funding address: {}", borrower.funding_address());
    println!();
    println!("Message for Firefish:\n{}", message);
}

fn offer_assign(mut args: std::env::ArgsOs) {
    use firefish_core::contract::context;
    use firefish_core::contract::pub_keys::ContractNumber;

    let key_file = args.next()
        .expect("missing key file");
    let state_file = args.next()
        .expect("missing state file");
    let key_bytes = std::fs::read(key_file).expect("failed to read offer");
    let (prefund_key, escrow_key, network) = if key_bytes.len() != 64 {
        if key_bytes.starts_with(b"xprv") || key_bytes.starts_with(b"tprv") {
            let derive_path = args.next()
                .expect("missing derivation path");
            let derive_path = derive_path.into_string()
                .expect("derivation path is not UTF-8")
                .parse::<bitcoin::bip32::DerivationPath>()
                .expect("invalid derivation path");

            let key_str = std::str::from_utf8(&key_bytes).expect("xpriv is not UTF-8");
            let xpriv = key_str.parse::<bitcoin::bip32::Xpriv>()
                .expect("failed to parse xpriv");
            let prefund_deriv_path = derive_path.extend(&[context::Prefund::CHILD_NUMBER]);
            let escrow_deriv_path = derive_path.extend(&[context::Escrow::CHILD_NUMBER]);
            let prefund_key = xpriv.derive_priv(&SECP256K1, &prefund_deriv_path)
                .expect("failed to derive key");
            let escrow_key = xpriv.derive_priv(&SECP256K1, &escrow_deriv_path)
                .expect("failed to derive key");

            (prefund_key.to_keypair(&SECP256K1), escrow_key.to_keypair(&SECP256K1), Some(xpriv.network))
        } else {
            panic!("invalid key file");
        }
    } else {
        let prefund_key = Keypair::from_seckey_slice(SECP256K1, &key_bytes[..32])
            .expect("invalid key file");
        let escrow_key = Keypair::from_seckey_slice(SECP256K1, &key_bytes[32..])
            .expect("invalid key file");
        (prefund_key, escrow_key, None)
    };
    let offer = load_offer(&mut args);
    if let Some(network) = network {
        if network.is_mainnet() && offer.escrow.network != bitcoin::Network::Bitcoin || !network.is_mainnet() && offer.escrow.network == bitcoin::Network::Bitcoin {
            panic!("networks don't match {:?} and {}", network, offer.escrow.network)
        }
    }
    let state = Ted::init(prefund_key, escrow_key, offer)
        .expect("The keys don't match any role in the offer");
    let mut bytes = Vec::new();
    state.serialize(&mut bytes);
    write_non_existing(&state_file, &bytes);
}

fn offer(mut args: std::env::ArgsOs) {
    let command = args.next()
        .expect("missing subcommand (create, decode, accept)")
        .into_string()
        .expect("unrecognized command");

    match &*command {
        "create" => offer_create(args),
        "decode" => offer_decode(args),
        "accept" => offer_accept(args),
        "assign" => offer_assign(args),
        _ => panic!("unknown command \"{}\"", command),
    }
}

fn escrow_init_from_prefund(mut args: std::env::ArgsOs) {
    use bitcoin::hashes::hex::FromHex;
    use bitcoin::consensus::Decodable;
    use bitcoin::blockdata::FeeRate;

    let state_file = args.next().expect("missing state file");
    let escrow_fee_rate = args.next()
        .expect("missing fee rate")
        .into_string()
        .expect("fee rate is not UTF-8")
        .parse::<u64>()
        .expect("invalid fee rate");
    let finalization_fee_rate = args.next()
        .expect("missing fee rate")
        .into_string()
        .expect("fee rate is not UTF-8")
        .parse::<u64>()
        .expect("invalid fee rate");
    let fee_bump_address = args.next()
        .expect("missing fee bump address")
        .into_string()
        .expect("fee bump address is not UTF-8")
        .parse::<bitcoin::Address<_>>()
        .expect("invalid fee bump address");
    let state_bytes = std::fs::read(&state_file).expect("failed to read state file");
    let state = participant::borrower::WaitingForFunding::deserialize(&mut &*state_bytes).expect("invalid state file");

    let fee_bump_address = fee_bump_address
        .require_network(state.network())
        .expect("The fee bump address belongs to a different network");
    let mut transactions = String::new();
    std::io::stdin().read_to_string(&mut transactions).expect("Failed to read stdin as UTF-8 string");
    if transactions.ends_with('\n') {
        transactions.pop();
    }
    // using awful bitcoin hex API because there's nothing better today.
    let transactions_bytes = Vec::from_hex(&transactions).expect("invalid hex");
    let mut transaction_bytes = &*transactions_bytes;
    let mut transactions = Vec::new();
    while !transaction_bytes.is_empty() {
        let transaction = bitcoin::Transaction::consensus_decode(&mut transaction_bytes)
            .expect("invalid transaction");
        transactions.push(transaction);
    }

    let params = participant::borrower::MandatoryFundingParams {
        transactions,
        escrow_fee_rate: FeeRate::from_sat_per_vb(escrow_fee_rate).expect("fee rate too high"),
        finalization_fee_rate: FeeRate::from_sat_per_vb(finalization_fee_rate).expect("fee rate too high"),
    };
    let mut funding = params.into_funding();
    let fee_bump_txout = TxOut::minimal_non_dust(fee_bump_address.script_pubkey());
    funding.repayment_extra_outputs.push(fee_bump_txout.clone());
    funding.recover_extra_outputs.push(fee_bump_txout);
    let mut message = Vec::new();
    let state = match state.funding_received(funding, &mut message) {
        Ok(state) => state,
        Err((_, error)) => panic!("funding error: {:?}", error),
    };
    // Reuse allocation :)
    let mut state_bytes = state_bytes;
    state_bytes.clear();
    state.serialize_with_header(&mut state_bytes);
    atomic_update(&state_file, &state_bytes);
    let message = base64::encode(message);
    println!("Message for Firefish (TedSig):\n{}", message);
}

fn write_non_existing(path: &std::ffi::OsStr, data: &[u8]) {
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .unwrap_or_else(|error| panic!("failed to open {:?}: {:?}", path, error));
    file.write_all(data).expect("failed to write");
}

fn atomic_update(path: &std::ffi::OsStr, data: &[u8]) {
    let mut tmp_state_file = path.to_owned();
    tmp_state_file.push(".tmp");
    // we want to call sync, so we create `File` manually
    let mut file = std::fs::File::create(&tmp_state_file).expect("failed to open temporary state file");
    file.write_all(&data).expect("failed to write new state");
    file.sync_data().expect("failed to ensure the file is on disk");
    drop(file);
    std::fs::rename(tmp_state_file, &path).expect("failed to commit the state file");
}

fn prefund_decode(mut args: std::env::ArgsOs) {
    let state_file = args.next().expect("missing state file");
    let state_bytes = std::fs::read(&state_file).expect("failed to read state file");
    let state = participant::borrower::WaitingForFunding::deserialize(&mut &*state_bytes).expect("invalid state file");

    println!("Funding address: {}", state.funding_address());
}

fn prefund_set_spend_info(mut args: std::env::ArgsOs) {
    let state_file = args.next().expect("missing state file");
    let state_bytes = std::fs::read(&state_file).expect("failed to read state file");
    let state = Ted::<escrow::ReceivingBorrowerInfo<participant::TedO>, escrow::ReceivingBorrowerInfo<participant::TedP>>::deserialize(&mut &*state_bytes).expect("invalid state file");

    let mut message = Vec::new();
    std::io::stdin().read_to_end(&mut message).expect("Failed to read stdin borrower spend info");
    if message.ends_with(b"\n") {
        message.pop();
    }
    let message_bytes = base64::decode(&message).expect("failed to decode the message");
    let borrower_info = prefund::BorrowerSpendInfo::deserialize(&mut &*message_bytes)
        .expect("invalid borrower spend info");
    let new_state = state.prefund_borrower_info(borrower_info).unwrap_or_else(|(_, error)| panic!("can't set borrower info: {:?}", error));
    message.clear();
    new_state.serialize(&mut message);
    atomic_update(&state_file, &message);
}

fn prefund_cancel(mut args: std::env::ArgsOs) {
    use bitcoin::hashes::hex::FromHex;
    use bitcoin::consensus::Decodable;

    let state_file = args.next().expect("missing state file");
    let state_bytes = std::fs::read(&state_file).expect("failed to read state file");
    let state = participant::borrower::State::deserialize(&mut &*state_bytes).expect("invalid state file");
    let fee_rate = args.next()
        .expect("missing fee rate")
        .into_string()
        .expect("fee rate is not UTF-8")
        .parse()
        .expect("invalid fee rate");
    let fee_rate = bitcoin::blockdata::FeeRate::from_sat_per_vb(fee_rate).expect("fee rate too high");

    let mut transactions = String::new();
    std::io::stdin().read_to_string(&mut transactions).expect("Failed to read stdin as UTF-8 string");
    if transactions.ends_with('\n') {
        transactions.pop();
    }
    // using awful bitcoin hex API because there's nothing better today.
    let transactions_bytes = Vec::from_hex(&transactions).expect("invalid hex");
    let mut transaction_bytes = &*transactions_bytes;
    let mut transactions = Vec::new();
    while !transaction_bytes.is_empty() {
        let transaction = bitcoin::Transaction::consensus_decode(&mut transaction_bytes)
            .expect("invalid transaction");
        transactions.push(transaction);
    }
    let height = bitcoin::locktime::absolute::Height::ZERO;
    let delay = participant::borrower::RelativeDelay::Zero;
    let tx = state.funding_cancel(transactions, fee_rate, height, delay).expect("failed to construct cancel transaction");
    let tx = bitcoin::consensus::encode::serialize_hex(&tx);
    println!("{}", tx);
}

fn prefund(mut args: std::env::ArgsOs) {
    let command = args.next()
        .expect("missing subcommand (decode)")
        .into_string()
        .expect("unrecognized command");

    match &*command {
        "decode" => prefund_decode(args),
        "set-spend-info" => prefund_set_spend_info(args),
        "cancel" => prefund_cancel(args),
        _ => panic!("unknown command \"{}\"", command),
    }
}

fn escrow_sign_from_prefund(mut args: std::env::ArgsOs) {
    use std::io::BufRead;

    let state_file = args.next().expect("missing state file");
    let state_bytes = std::fs::read(&state_file).expect("failed to read state file");
    let state = escrow::ReceivingEscrowSignature::<participant::Borrower>::deserialize_with_header(&mut &*state_bytes)
        .expect("invalid state");

    let msg1 = args.next()
        .expect("missing first signature")
        .into_string()
        .expect("could not convert message to a valid UTF8 string");
    let mut msg1 = base64::decode(&msg1).expect("failed to decode message");
    
    let msg2 = args.next()
        .expect("missing second signature")
        .into_string()
        .expect("could not convert second message to a valid UTF8 string");
    let mut msg2 = base64::decode(&msg2).expect("failed to decode message");

    if msg1[0] == 7 {
        std::mem::swap(&mut msg1, &mut msg2);
    }
    let ted_o_sigs = escrow::TedOSignatures::deserialize(&mut &*msg1).expect("failed to deserialize TED-O signatures");
    let ted_p_sigs = escrow::TedPSignatures::deserialize(&mut &*msg2).expect("failed to deserialize TED-P signatures");
    let state = match state.verify_signatures(ted_o_sigs, ted_p_sigs) {
        Ok(state) => state,
        Err((_, error)) => panic!("invalid signatures: {:?}", error),
    };
    println!();
    println!("===========================");
    println!();
    println!("IMPORTANT: You MUST backup the following transaction!");
    let recover = bitcoin::consensus::encode::serialize_hex(state.recover_tx());
    println!("{}", recover);
    println!();
    println!("===========================");
    println!();
    println!("Write \"I have backed it up\" (without quotes once you did");
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    loop {
        let mut line = lines.next().expect("transaction not backed up, aborting").expect("IO error");
        if line.ends_with('\n') {
            line.pop();
        }
        if line == "I have backed it up" {
            break;
        }
        println!("You didn't back it up yet?");
    }
    let state = match state.assemble_escrow() {
        Ok(state) => state,
        Err((_, error)) => panic!("Recover signatures are OK but the escrow signatures are invalid, {:?}", error),
    };
    println!();
    println!("===========================");
    println!("Done!");
    println!();
    println!("Broadcast this transaction:");
    println!("{}", bitcoin::consensus::encode::serialize_hex(state.tx_escrow()));
}

fn escrow_presign(mut args: std::env::ArgsOs) {
    let state_file = args.next()
        .expect("missing state file");
    let state_bytes = std::fs::read(&state_file).expect("can't read state file");
    let state = Ted::<escrow::ReceivingBorrowerInfo<participant::TedO>, escrow::ReceivingBorrowerInfo<participant::TedP>>::deserialize(&mut &*state_bytes).expect("invalid state file");

    let mut buf = Vec::new();
    std::io::stdin().read_to_end(&mut buf).expect("failed to read message from stdin");
    if buf.last() == Some(&b'\n') {
        buf.pop();
    }
    let bytes = base64::decode(buf).expect("invlid message encoding");
    let message = contract::escrow::BorrowerInfoMessage::deserialize(&mut &*bytes)
        .expect("invalid message from borrower");
    let escrow = match &state {
        Ted::O(state) => &state.params,
        Ted::P(state) => &state.params,
    };
    let info = message.borrower_info.validate(escrow).expect("invalid borrower information");
    let transactions = state.borrower_info(info);
    transactions.verify_borrower(&message.signatures).expect("transactions have invalid signature(s)");
    println!("{}", transactions.explain());
    let mut serialized_signatures = Vec::new();
    let state = state.set_and_sign_transactions(transactions, message.signatures, &mut serialized_signatures);
    let mut state_bytes = Vec::new();
    state.serialize(&mut state_bytes);
    atomic_update(&state_file, &state_bytes);
    let encoded_signatures = base64::encode(serialized_signatures);
    let txid = match state {
        Ted::O(state) => state.escrow_txid(),
        Ted::P(state) => state.escrow_txid(),
    };
    println!("Watch for this transaction to confirm: {}", txid);
    println!("Signatures:\n{}", encoded_signatures);
}

fn escrow_repayment(mut args: std::env::ArgsOs) {
    let state_file = args.next()
        .expect("missing state file");
    let state_bytes = std::fs::read(&state_file).expect("can't read state file");
    let mut state = escrow::WaitingForEscrowConfirmation::<participant::TedP>::deserialize_with_header(&mut &*state_bytes).expect("invalid state");
    let ted_o_sigs = escrow::TedOSignatures::deserialize(&mut &*base64_bytes_from_stdin())
        .expect("invalid message from TED-O");
    let tx = bitcoin::consensus::encode::serialize_hex(&mut state.sign_repayment(&ted_o_sigs.repayment));
    println!("{}", tx);
}

fn escrow_default(mut args: std::env::ArgsOs) {
    let state_file = args.next()
        .expect("missing state file");
    let state_bytes = std::fs::read(&state_file).expect("can't read state file");
    let mut state = escrow::WaitingForEscrowConfirmation::<participant::TedP>::deserialize_with_header(&mut &*state_bytes).expect("invalid state");
    let ted_o_sigs = escrow::TedOSignatures::deserialize(&mut &*base64_bytes_from_stdin())
        .expect("invalid message from TED-O");
    let tx = bitcoin::consensus::encode::serialize_hex(&mut state.sign_default(&ted_o_sigs.default));
    println!("{}", tx);
}

fn escrow_liquidation(mut args: std::env::ArgsOs) {
    use escrow::WaitingForEscrowConfirmation;

    let state_file = args.next()
        .expect("missing state file");
    let state_bytes = std::fs::read(&state_file).expect("can't read state file");
    let state = Ted::<WaitingForEscrowConfirmation<participant::TedO>, WaitingForEscrowConfirmation<participant::TedP>>::deserialize(&mut &*state_bytes).expect("invalid state");
    match state {
        Ted::O(state) => {
            let sig = state.ted_o_sign_liquidation();
            println!("Signature:\n{}", base64::encode(sig.as_ref()));
        },
        Ted::P(mut state) => {
            let ted_o_sig = secp256k1::schnorr::Signature::from_slice(&base64_bytes_from_stdin())
                .expect("invalid message from TED-O");
            let tx = bitcoin::consensus::encode::serialize_hex(&mut state.sign_liquidation(&ted_o_sig));
            println!("{}", tx);
        },
    }
}

fn escrow(mut args: std::env::ArgsOs) {
    let command = args.next()
        .expect("missing subcommand (init-from-prefund, presign, sign-from-prefund)")
        .into_string()
        .expect("unrecognized command");

    match &*command {
        "init-from-prefund" => escrow_init_from_prefund(args),
        "sign-from-prefund" => escrow_sign_from_prefund(args),
        "presign" => escrow_presign(args),
        "repayment" => escrow_repayment(args),
        "default" => escrow_default(args),
        "liquidation" => escrow_liquidation(args),
        _ => panic!("unknown command \"{}\"", command),
    }
}

fn key_gen(mut args: std::env::ArgsOs) {
    let role = args.next()
        .expect("missing role (ted-o or ted-p)")
        .into_string()
        .expect("invalid role (must be ted-o or ted-p)");
    let key_file = args.next()
        .expect("missing key file");

    let symbol = match &*role {
        "ted-o" => 'o',
        "ted-p" => 'p',
        _ => panic!("invalid role (must be ted-o or ted-p): {}", role),
    };

    let prefund_key_pair = Keypair::new(SECP256K1, &mut secp256k1::rand::thread_rng());
    let escrow_key_pair = Keypair::new(SECP256K1, &mut secp256k1::rand::thread_rng());

    let mut secrets = [0u8; 64];
    secrets[..32].copy_from_slice(&prefund_key_pair.secret_bytes());
    secrets[32..].copy_from_slice(&escrow_key_pair.secret_bytes());

    write_non_existing(&key_file, &secrets);

    println!("ffa{}k{}{}", symbol, prefund_key_pair.x_only_public_key().0, escrow_key_pair.x_only_public_key().0);
}

fn key_derive_public(mut args: std::env::ArgsOs) {
    use firefish_core::contract::pub_keys::PubKey;
    use firefish_core::contract::context;

    let role = args.next()
        .expect("missing role (ted-o or ted-p)")
        .into_string()
        .expect("invalid role (must be ted-o or ted-p)");
    let xpub = args.next()
        .expect("missing xpub")
        .into_string()
        .expect("xpub is not UTF-8")
        .parse::<bitcoin::bip32::Xpub>()
        .expect("failed to parse xpup");

    let derive_path = args.next()
        .expect("missing derivation path")
        .into_string()
        .expect("derivation path is not UTF-8")
        .parse::<bitcoin::bip32::DerivationPath>()
        .expect("invalid derivation path");

    let symbol = match &*role {
        "ted-o" => 'o',
        "ted-p" => 'p',
        _ => panic!("invalid role (must be ted-o or ted-p): {}", role),
    };

    let prefund_key = PubKey::<(), context::Prefund>::from_xpub(&xpub, &derive_path);
    let escrow_key = PubKey::<(), context::Escrow>::from_xpub(&xpub, &derive_path);

    println!("ffa{}k{}{}", symbol, prefund_key.as_x_only(), escrow_key.as_x_only());
}

fn key_gen_xpriv(mut args: std::env::ArgsOs) {
    let network = args
        .next()
        .expect("missing network")
        .into_string()
        .expect("network is not UTF-8")
        .parse::<bitcoin::Network>()
        .expect("invalid network");
    let key_file = args.next()
        .expect("missing key file");
    let mnemonic = match args.next() {
        Some(seed) => {
            seed.into_string().expect("seed is not UTF-8").parse().expect("invalid seed")
        },
        None => {
            let entropy = secp256k1::rand::random::<[u8; 16]>();
            bip39::Mnemonic::from_entropy(&entropy).expect("correct entropy length")
        },
    };
    let seed = mnemonic.to_seed("");
    let xpriv = bitcoin::bip32::Xpriv::new_master(network, &seed).expect("failed to create xpriv");
    let xpub = bitcoin::bip32::Xpub::from_priv(&SECP256K1, &xpriv);
    println!("seed: {}", mnemonic);
    println!("xpub: {}", xpub);
    write_non_existing(&key_file, xpriv.to_string().as_bytes())
}

fn key(mut args: std::env::ArgsOs) {
    let command = args.next()
        .expect("missing subcommand (gen)")
        .into_string()
        .expect("unrecognized command");

    match &*command {
        "gen" => key_gen(args),
        "gen-xpriv" => key_gen_xpriv(args),
        "derive-pub" => key_derive_public(args),
        _ => panic!("unknown command \"{}\"", command),
    }
}

fn print(mut args: std::env::ArgsOs) {
    let subject = args.next()
        .expect("missing subject")
        .into_string()
        .expect("unrecognized subject");

    match &*subject {
        "api-version" => println!("1"),
        _ => panic!("unknown subject \"{}\"", subject),
    }
}

fn base64_bytes_from_stdin() -> Vec<u8> {
    let mut buf = Vec::new();
    std::io::stdin().read_to_end(&mut buf).expect("failed to read offer from stdin");
    if buf.last() == Some(&b'\n') {
        buf.pop();
    }
    base64::decode(buf).expect("failed to decode the base64 offer bytes")
}

fn load_offer(args: &mut std::env::ArgsOs) -> contract::offer::Offer {
    let bytes = match args.next() {
        Some(path) => std::fs::read(&path).expect("failed to read offer"),
        None => base64_bytes_from_stdin(),
    };
    contract::offer::Offer::deserialize(&mut &*bytes).expect("failed to deserialize offer")
}

fn main() {
    let mut args = std::env::args_os();
    let _program_name = args.next().expect("missing program name");
    let command = args.next()
        .expect("missing subcommand (offer, prefund)")
        .into_string()
        .expect("unrecognized command");

    match &*command {
        "offer" => offer(args),
        "prefund" => prefund(args),
        "escrow" => escrow(args),
        "key" => key(args),
        "print" => print(args),
        _ => panic!("unknown command \"{}\"", command),
    }
}
