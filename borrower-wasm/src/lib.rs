use wasm_bindgen::prelude::*;
use bitcoin::{Address, Sequence};
use firefish_core::contract::{self, participant};
use secp256k1::{Keypair, SECP256K1};

/// Sets up error handling, call this after initializing WASM module.
#[wasm_bindgen]
pub fn set_panic_hook() {
    console_error_panic_hook::set_once();
}

/// Represents offer: contract initialization data.
#[wasm_bindgen]
pub struct Offer(firefish_core::contract::offer::Offer);

#[wasm_bindgen]
impl Offer {
    /// Parses the offer from base64-encoded string.
    pub fn parse(offer_base64: &str) -> Result<Offer, JsValue> {
        let bytes = base64::decode(offer_base64).map_err(into_string)?;
        let offer = contract::offer::Offer::deserialize(&mut &*bytes).map_err(into_debug_string)?;
        Ok(Offer(offer))
    }

    /// Creates borrower state using the offer and return address.
    ///
    /// If this method returns an error it means the return address is invalid.
    pub fn accept(&self, return_address: &str) -> Result<Borrower, JsValue> {
        let return_address = return_address.parse::<Address<_>>()
            .map_err(into_string)?
            .require_network(self.0.escrow.network)
            .map_err(into_string)?;
        let key_pair = Keypair::new(SECP256K1, &mut secp256k1::rand::thread_rng());

        let params = participant::borrower::MandatoryPrefundParams {
            key_pair,
            lock_time: Sequence::from_height(144 * 7), // 7 days
            return_script: return_address.script_pubkey(),
        };

        let borrower = participant::borrower::init_prefund(self.0.clone(), params.into_params());

        let mut message = Vec::new();
        borrower.borrower_info().serialize(&mut message);
        let message = base64::encode(&message);

        Ok(Borrower {
            state: Some(participant::borrower::State::WaitingForFunding(borrower)),
            message: Some(message),
            cancel_tx: None,
        })
    }
}

/// Contains all borrower data.
#[wasm_bindgen]
#[derive(Debug)]
pub struct Borrower {
    // None means message_received panicked
    state: Option<participant::borrower::State>,
    message: Option<String>,
    cancel_tx: Option<bitcoin::Transaction>
}

struct TakenStateInner<'a, S, F> {
    state: S,
    map: F,
    set: &'a mut Option<participant::borrower::State>,
}

// Guard for state to restore it in case of failure
struct TakenState<'a, S, F> where F: FnOnce(S) -> participant::borrower::State {
    inner: Option<TakenStateInner<'a, S, F>>,
}

impl<'a, S, F> TakenState<'a, S, F> where F: Fn(S) -> participant::borrower::State {
    fn new(state: S, set: &'a mut Option<participant::borrower::State>, map: F) -> Self {
        TakenState {
            inner: Some(TakenStateInner {
                state,
                set,
                map,
            })
        }
    }

    fn try_map<E, F2>(mut self, map: F2) -> Result<(), E> where F2: FnOnce(S) -> Result<participant::borrower::State, (S, E)> {
        let mut inner = self.inner.take().expect("Attempt to map after successful transfer");
        match map(inner.state) {
            Ok(new) => {
                *inner.set = Some(new);
                Ok(())
            },
            Err((old, error)) => {
                inner.state = old;
                self.inner = Some(inner);
                Err(error)
            }
        }
    }

    fn state(&self) -> &S {
        &self.inner.as_ref().unwrap().state
    }

    fn state_mut(&mut self) -> &mut S {
        &mut self.inner.as_mut().unwrap().state
    }
}

impl<'a, S, F> Drop for TakenState<'a, S, F> where F: FnOnce(S) -> participant::borrower::State {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            *inner.set = Some((inner.map)(inner.state));
        }
    }
}

#[wasm_bindgen]
impl Borrower {
	/// Called when a new message from Firefish was received.
	///
	/// The consumer of this library has to subscribe to incoming messages from Firefish API and feed them into this function.
	/// It is unspecified when and how many messages may be received.
	/// After this method returns messageToSend() MUST be called to potentially obtain a response.
    ///
    /// If this function returns an error (exception) the message was invalid and the error should
    /// be logged.
    pub fn message_received(&mut self, message: &str) -> Result<(), JsValue> {
        use contract::escrow::TedSignatures;

        let bytes = base64::decode(message).map_err(into_string)?;

        match self.state.take().expect("use of invalidated Borrower") {
            participant::borrower::State::WaitingForFunding(state) => {
                let state = TakenState::new(state, &mut self.state, participant::borrower::State::WaitingForFunding);
                let hints = contract::offer::EscrowHints::deserialize(&mut &*bytes)
                    .map_err(into_debug_string)?;
                let cancel_fee_rate = bitcoin::FeeRate::from_sat_per_vb(50 + hints.fee_rate.to_sat_per_vb_ceil()).unwrap();
                let funding = participant::borrower::Funding::from_hints(hints);
                let mut response = Vec::new();
                let txs = funding.mandatory.transactions.clone();
                let height = bitcoin::absolute::Height::from_consensus(0).unwrap();
                let delay = participant::borrower::RelativeDelay::Height(144 * 7);
                let cancel_tx = state.state().funding_cancel(txs, cancel_fee_rate, height, delay)
                    .map_err(into_debug_string)?;
                self.cancel_tx = Some(cancel_tx);
                state.try_map(|state| {
                    state.funding_received(funding, &mut response)
                        .map(|state| participant::borrower::State::ReceivingEscrowSignature { state, received: None })
                })
                    .map_err(into_debug_string)?;
                self.message = Some(base64::encode(&response));
                Ok(())
            },
            participant::borrower::State::ReceivingEscrowSignature { state, received } => {
                let mut state = TakenState::new((state, received), &mut self.state, |(state, received)| participant::borrower::State::ReceivingEscrowSignature { state, received });
                let message = TedSignatures::deserialize(&mut &*bytes)
                    .map_err(into_debug_string)?
                    .ok_or("empty message")?;
                let received = &mut state.state_mut().1;
                match (received.take(), message) {
                    (None, message) => {
                        *received = Some(message);
                        self.message = None;
                        Ok(())
                    },
                    (Some(TedSignatures::TedO(ted_o)), TedSignatures::TedP(ted_p)) |
                     (Some(TedSignatures::TedP(ted_p)), TedSignatures::TedO(ted_o)) => {
                         state.try_map(|state| {
                             state.0.verify_signatures(ted_o, ted_p)
                                 .map(participant::borrower::State::SignaturesVerified)
                                 .map_err(|(old, err)| ((old, None), err))
                         })
                         .map_err(into_debug_string)?;
                         Ok(())
                     },
                    (Some(old @ TedSignatures::TedO(_)), TedSignatures::TedO(_)) | (Some(old @ TedSignatures::TedP(_)), TedSignatures::TedP(_)) => {
                        *received = Some(old);
                        Err("message already received".into())
                    },
                }
            },
            state @ participant::borrower::State::SignaturesVerified(_) => {
                self.state = Some(state);
                Err("No message was expected in this state".into())
            },
            state @ participant::borrower::State::EscrowSigned(_) => {
                self.state = Some(state);
                Err("No message was expected in this state".into())
            },
        }
    }

	/// Call this when the user confirmed he backed up the recover transaction.
	///
	/// The call to this function marks that it's safe to continue.
	/// This will produce a new message for counterparties.
	///
	/// Spurious calls to this function may lead to loss of funds.
	///
	/// This method may only be called in RecoverTxSigned state!
	/// Attempt to call it in any other state will throw an exception.
    pub fn recover_tx_backed_up(&mut self) -> Result<(), JsValue> {
        match self.state.take().expect("use of invalid state") {
            participant::borrower::State::SignaturesVerified(state) => {
                let state = TakenState::new(state, &mut self.state, participant::borrower::State::SignaturesVerified);
                let mut message = Vec::new();
                state.try_map(|state| {
                    let new_state = state.assemble_escrow()?;
                    new_state.serialize_broadcast_request(&mut message);

                    Ok(participant::borrower::State::EscrowSigned(new_state))
                }).map_err(into_debug_string)?;
                self.message = Some(base64::encode(&message));
                Ok(())
            },
            state => {
                self.state = Some(state);
                panic!("attempt to call recover_tx_backed_up in unusable state");
            },
        }
    }

	/// Returns the message that needs to be sent to Firefish.
	///
	/// This message may be available after these operations:
	///
	/// * After this class is created
	/// * After the call to message_received() returns
	/// * After the call to recover_tx_backed_up() returns
	///
	/// There will never be a new message "out of thin air" - IOW, there's no background thread/task generating messages.
	/// Therefore polling this method repeatedly is just wasted CPU time.
	///
	/// If a non-null message is returned it must be sent to Firefish.
	/// The message is present until any of the methods mentioned above is called, so it can be re-sent if required (e.g. if it was lost).
	/// Returned null shoul be silently ignored.
    pub fn message_to_send(&self) -> Option<String> {
        self.message.clone()
    }

	/// Returns the invoice for the user to pay.
	///
	/// This method may only be called in PrefundReady state!
	/// Attempt to call it in any other state will throw an exception.
    ///
    /// `reserve_sats` is the amount to reserve on top of collateral to pay for miner fees.
    pub fn compute_prefund_invoice(&self, reserve_sats: u64) -> Invoice {
        let (address, liq_amount) = match &self.state.as_ref().expect("attempt to use invalid state") {
            participant::borrower::State::WaitingForFunding(state) => (state.funding_address(), state.liquidator_amount()),
            _ => panic!("invalid state"),
        };

        let amount = liq_amount + bitcoin::Amount::from_sat(reserve_sats);

        let mut uri = bip21::Uri::new(address);
        uri.amount = Some(amount);
        uri.label = Some("Firefish smart contract".into());
        uri.message = Some("Deposit for a loan from Firefish".into());
        Invoice(uri)
    }

    /// Serializes the whole borrower state.
    pub fn serialize_state(&self) -> String {
        let mut buf = Vec::new();
        self.state.as_ref().expect("attempt to use invalid state").serialize(&mut buf);
        base64::encode(&buf)
    }

    /// Deserializes the whole borrower state.
    pub fn deserialize_state(state: &str) -> Result<Borrower, JsValue> {
        let bytes = base64::decode(state).map_err(into_string)?;
        let state = participant::borrower::State::deserialize(&mut &*bytes).map_err(into_debug_string)?;
        Ok(Borrower {
            state: Some(state),
            message: None,
            cancel_tx: None,
        })
    }

    /// Returns a string containing debug representation of the current state.
    pub fn debug_string_with_private_keys(&self) -> String {
        match self.state.as_ref() {
            Some(state) if state.network() != bitcoin::Network::Regtest => panic!("debugging would leak private keys"),
            _ => format!("{:?}", self),
        }
    }

    /// Returns the current state.
    pub fn state(&self) -> BorrowerState {
        match self.state.as_ref().expect("use of invalid borrower") {
            participant::borrower::State::WaitingForFunding(_) => BorrowerState::PrefundReady,
            participant::borrower::State::ReceivingEscrowSignature { .. } => BorrowerState::AwaitingTxSignatures,
            participant::borrower::State::SignaturesVerified(_) => BorrowerState::RecoverTxSigned,
            participant::borrower::State::EscrowSigned(_) => BorrowerState::EscrowTxSigned,
        }
    }

    /// Returns base64-encoded cancel transaction.
    ///
    /// This transaction can be used in disaster recovery scenario if everything else failed.
    /// It's more expensive, si it should really be used as a last resort only.
    ///
    /// The transaction is **not** available after restoring the state, so it's best to store it
    /// *before* backing up the state.
    ///
    /// The transaction becomes available after entering AwaitingTxSignatures state.
    pub fn pre_cancel_transaction(&self) -> Result<String, JsValue> {
        let cancel_tx = self.cancel_tx
            .as_ref()
            .ok_or("pre-cancel transaction unavailable")?;
        Ok(bitcoin::consensus::encode::serialize_hex(cancel_tx))
    }

    /// Returns hex-encoded recover transaction.
    ///
    /// This transaction can be used to return satoshis back to the borrower after the time lock
    /// expires.
    pub fn recover_transaction(&self) -> Result<String, JsValue> {
        match self.state.as_ref().expect("use of invalid borrower") {
            participant::borrower::State::SignaturesVerified(state) => {
                Ok(bitcoin::consensus::encode::serialize_hex(state.recover_tx()))
            },
            participant::borrower::State::EscrowSigned(state) => {
                Ok(bitcoin::consensus::encode::serialize_hex(&state.recover))
            },
            _ => Err("recover_transaction called in invalid state".into()),
        }
    }

    /// Cancels the prefund.
    ///
    /// Parameters:
    ///
    /// * transactions - an array of hex-encoded bitcoin transactions that send satoshis to
    ///                  prefund.
    /// * fee_rate_sat_per_vb - fee rate in sat/vB (satoshis per virtual byte)
    pub fn cancel_prefund(&self, transactions: js_sys::Array, fee_rate_sat_per_vb: u64) -> Result<String, JsValue> {
        use bitcoin::hashes::hex::FromHex;
        use bitcoin::consensus::Decodable;
        use firefish_core::contract::participant::borrower::RelativeDelay;

        let fee_rate = bitcoin::FeeRate::from_sat_per_vb(fee_rate_sat_per_vb).ok_or("fee rate too high")?;
        let transactions = transactions.iter().map(|tx| {
            let tx_bytes = Vec::from_hex(&tx.as_string().unwrap()).map_err(into_debug_string)?;
            bitcoin::Transaction::consensus_decode(&mut &*tx_bytes).map_err(into_debug_string)
        })
        .collect::<Result<_, _>>()?;
        self.state.as_ref().unwrap().funding_cancel(transactions, fee_rate, bitcoin::absolute::Height::ZERO, RelativeDelay::Zero)
            .map(|tx| bitcoin::consensus::encode::serialize_hex(&tx))
            .map_err(into_debug_string)
            .map_err(Into::into)
    }

    /// Changes the state back to `PrefundReady` forgetting all steps since that state.
    ///
    /// The offer has to be the original one used to create this state.
    /// The behavior is **UNSPECIFIED** if a different offer is passed.
    pub fn reset(&mut self, offer: Offer) {
        self.state.as_mut().unwrap().reset(offer.0);
    }
}

/// The state of borrower contract
#[wasm_bindgen]
pub enum BorrowerState {
	/// All prefund keys are ready, prefund invoice can be computed.
	///
	/// The prefund invoice should be shown to the user. And the application should wait for funding.
	PrefundReady,

	/// The borrower is waiting for the counterparties to sign the recover transaction.
	///
	/// The transaction ID of prefund transaction is known at this point so the application should hide the invoice
	/// and display a message saying it's waiting for the counterparties.
	AwaitingTxSignatures,

	/// The recover TX is signed and it needs to be backed up.
	///
	/// The contract is almost ready.
	/// The user should be told to backup the recover transaction and confirm that he did so.
	/// The backup could also be sent to him via e-mail or other means.
	/// It does not contain any private keys but may leak some details about the contract.
	RecoverTxSigned,

	/// The escrow transaction was signed, fiat should arrive after it is confirmed in the chain.
	///
	/// The application should show "all done" message.
	/// It may also show the escrow transaction ID and suggest to the user to check its state at his own node
	/// or a public chain explorer if he deosn't mind degradation of privacy.
	EscrowTxSigned,
}

/// A Bitcoin address and amount
#[wasm_bindgen]
pub struct Invoice(bip21::Uri<'static>);

#[wasm_bindgen]
impl Invoice {
	/// Returns BIP21 URI.
	///
	/// This can be used as a link which can open a supported wallet (if integrated into users OS/browser) with pre-filled amount and address.
	/// It can be technically shown to the user but it's not usual and may be confusing for some.
	///
	/// While it technically works in QR codes it is NOT optimized for them. qrCodeData() should be used for QR codes instead.
    pub fn uri(&self) -> String {
        self.0.to_string()
    }

	/// Returns string intended for putting into QR code.
	///
	/// This is technically an URI optimized for QR codes to make them smaller.
	/// However to make use of the optimization an appropriate QR code library has to be used.
	///
	/// Note that this may not be compatible with some ancient wallets.
	/// For a table and in-depth discussion on compatibility issues see
    /// https://github.com/btcpayserver/btcpayserver/issues/2110
	///
	/// This SHOULD NOT be displayed to the user.
    pub fn qr_code_data(&self) -> String {
        format!("{:#}", self.0)
    }

	/// Returns string intended for displaying to the user.
	///
	/// This can be used to show the standalone address as a text to the user.
	///
	/// Note that the traditional showing of address and amount can lead to user errors (sending wrong amount).
	/// If possible it's better to discourage users from doing that and use URI or QR code instead.
    pub fn address(&self) -> String {
        self.0.address.to_string()
    }

	/// Returns the number of satoshis to send.
	///
	/// This is intended for showing to the user for informative purposes.
	/// The returned amount is guaranteed to be an integer larger than 0 and smaller or equal to 2 100 000 000 000 000.
	///
	/// Be careful when formatting this as bitcoins or non-satoshi units.
	/// Converting to float may lose precision and turn into a wrong value.
	/// E.g. This is the correct code for converting to bitcoins:
	/// Math.floor(number / 100000000).toString() + "." + (number % 100000000).toString().padStart(8, '0').replace(/0*$/, "")
    pub fn satoshis(&self) -> u64 {
        self.0.amount.expect("amount is alays Some").to_sat()
    }
}

// makes map_err simpler
fn into_string<T: core::fmt::Display>(val: T) -> String {
    val.to_string()
}

fn into_debug_string<T: core::fmt::Debug>(val: T) -> String {
    format!("{:?}", val)
}
