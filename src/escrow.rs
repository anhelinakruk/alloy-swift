use alloy::primitives::{Address, U256};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::rpc::types::{TransactionReceipt, TransactionRequest};
use alloy::sol;
use alloy::sol_types::SolCall;
use alloy::transports::http::reqwest::Url;
use alloy_network::TransactionBuilder;
use alloy_signer_local::{coins_bip39::English, MnemonicBuilder};
use std::future::Future;
use std::str::FromStr;

use crate::AlloyError;

sol! {
    #[sol(rpc)]
    contract Escrow {
        event EscrowCreated(uint256 indexed id, address indexed buyer, uint256 amount);

        function createEscrow() external payable returns (uint256 id);
        function joinEscrow(uint256 id) external;
        function cancelEscrow(uint256 id) external;
        function markShipped(uint256 id) external;
        function confirmReceipt(uint256 id) external;
        function refundExpired(uint256 id) external;
        function autoComplete(uint256 id) external;
        function raiseDispute(uint256 id) external;
        function withdraw() external;

        function nextId() external view returns (uint256);
        function pendingWithdrawals(address who) external view returns (uint256);
        function transactions(uint256 id) external view returns (
            address buyer,
            address seller,
            uint256 amount,
            uint8 state,
            uint256 shipDeadline,
            uint256 confirmDeadline
        );
    }
}

/// Mirrors `Escrow.State` in the contract; order must match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum EscrowState {
    Created,
    Funded,
    Shipped,
    Completed,
    Disputed,
    Refunded,
}

/// Every state-changing call that targets an existing escrow by id.
#[derive(Debug, Clone, Copy, uniffi::Enum)]
pub enum EscrowAction {
    Join,
    Cancel,
    MarkShipped,
    ConfirmReceipt,
    RefundExpired,
    AutoComplete,
    RaiseDispute,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct EscrowInfo {
    pub id: u64,
    pub buyer: String,
    pub seller: Option<String>,
    pub amount_wei: String,
    pub state: EscrowState,
    /// Unix seconds; `None` until the contract sets it.
    pub ship_deadline: Option<u64>,
    pub confirm_deadline: Option<u64>,
}

/// Buyer creates and funds an escrow; returns its id (the one shared in the link).
#[uniffi::export]
pub async fn create_escrow(
    mnemonic: &str,
    rpc_url: &str,
    contract: &str,
    amount_wei: &str,
) -> Result<u64, AlloyError> {
    block_on(async {
        let value = U256::from_str(amount_wei).map_err(provider_err)?;
        let tx = TransactionRequest::default()
            .with_to(parse_address(contract)?)
            .with_value(value)
            .with_input(Escrow::createEscrowCall {}.abi_encode());
        let receipt = send_tx(mnemonic, rpc_url, tx).await?;

        receipt
            .inner
            .logs()
            .iter()
            .find_map(|log| log.log_decode::<Escrow::EscrowCreated>().ok())
            .map(|log| log.inner.data.id.saturating_to::<u64>())
            .ok_or_else(|| provider_err("EscrowCreated event missing from receipt"))
    })
}

/// Runs one of the per-escrow actions; returns the transaction hash.
#[uniffi::export]
pub async fn escrow_action(
    mnemonic: &str,
    rpc_url: &str,
    contract: &str,
    action: EscrowAction,
    id: u64,
) -> Result<String, AlloyError> {
    block_on(async {
        let id = U256::from(id);
        let input = match action {
            EscrowAction::Join => Escrow::joinEscrowCall { id }.abi_encode(),
            EscrowAction::Cancel => Escrow::cancelEscrowCall { id }.abi_encode(),
            EscrowAction::MarkShipped => Escrow::markShippedCall { id }.abi_encode(),
            EscrowAction::ConfirmReceipt => Escrow::confirmReceiptCall { id }.abi_encode(),
            EscrowAction::RefundExpired => Escrow::refundExpiredCall { id }.abi_encode(),
            EscrowAction::AutoComplete => Escrow::autoCompleteCall { id }.abi_encode(),
            EscrowAction::RaiseDispute => Escrow::raiseDisputeCall { id }.abi_encode(),
        };
        let tx = TransactionRequest::default()
            .with_to(parse_address(contract)?)
            .with_input(input);
        Ok(send_tx(mnemonic, rpc_url, tx).await?.transaction_hash.to_string())
    })
}

/// Pulls the caller's whole pending balance out of the contract.
#[uniffi::export]
pub async fn withdraw(mnemonic: &str, rpc_url: &str, contract: &str) -> Result<String, AlloyError> {
    block_on(async {
        let tx = TransactionRequest::default()
            .with_to(parse_address(contract)?)
            .with_input(Escrow::withdrawCall {}.abi_encode());
        Ok(send_tx(mnemonic, rpc_url, tx).await?.transaction_hash.to_string())
    })
}

/// Plain ETH transfer with an exact wei amount; the chain id comes from the RPC.
#[uniffi::export]
pub async fn transfer_eth(
    mnemonic: &str,
    rpc_url: &str,
    to: &str,
    amount_wei: &str,
) -> Result<String, AlloyError> {
    block_on(async {
        let tx = TransactionRequest::default()
            .with_to(parse_address(to)?)
            .with_value(U256::from_str(amount_wei).map_err(provider_err)?);
        Ok(send_tx(mnemonic, rpc_url, tx).await?.transaction_hash.to_string())
    })
}

#[uniffi::export]
pub async fn get_escrow(rpc_url: &str, contract: &str, id: u64) -> Result<EscrowInfo, AlloyError> {
    block_on(async {
        let provider = ProviderBuilder::new().connect_http(parse_url(rpc_url)?);
        let escrow = Escrow::new(parse_address(contract)?, &provider);
        fetch_escrow(&escrow, id).await
    })
}

/// Every escrow where `address` is the buyer or the seller, newest first.
#[uniffi::export]
pub async fn list_escrows_for(
    rpc_url: &str,
    contract: &str,
    address: &str,
) -> Result<Vec<EscrowInfo>, AlloyError> {
    block_on(async {
        let me = parse_address(address)?;
        let provider = ProviderBuilder::new().connect_http(parse_url(rpc_url)?);
        let escrow = Escrow::new(parse_address(contract)?, &provider);
        let count = escrow
            .nextId()
            .call()
            .await
            .map_err(provider_err)?
            .saturating_to::<u64>();

        // ponytail: one RPC read per escrow ever created; switch to EscrowCreated/SellerJoined
        // logs or a backend index if the contract grows past a few hundred deals.
        let mut mine = Vec::new();
        for id in (0..count).rev() {
            let info = fetch_escrow(&escrow, id).await?;
            let seller = info.seller.as_deref().map(parse_address).transpose()?;
            if parse_address(&info.buyer)? == me || seller == Some(me) {
                mine.push(info);
            }
        }
        Ok(mine)
    })
}

#[uniffi::export]
pub async fn pending_withdrawal(
    rpc_url: &str,
    contract: &str,
    address: &str,
) -> Result<String, AlloyError> {
    block_on(async {
        let provider = ProviderBuilder::new().connect_http(parse_url(rpc_url)?);
        let escrow = Escrow::new(parse_address(contract)?, &provider);
        let owed = escrow
            .pendingWithdrawals(parse_address(address)?)
            .call()
            .await
            .map_err(provider_err)?;
        Ok(owed.to_string())
    })
}

/// Native ETH balance in wei.
#[uniffi::export]
pub async fn get_balance(rpc_url: &str, address: &str) -> Result<String, AlloyError> {
    block_on(async {
        let provider = ProviderBuilder::new().connect_http(parse_url(rpc_url)?);
        let balance = provider
            .get_balance(parse_address(address)?)
            .await
            .map_err(provider_err)?;
        Ok(balance.to_string())
    })
}

async fn fetch_escrow<P: Provider>(
    escrow: &Escrow::EscrowInstance<P>,
    id: u64,
) -> Result<EscrowInfo, AlloyError> {
    let t = escrow
        .transactions(U256::from(id))
        .call()
        .await
        .map_err(provider_err)?;
    if t.buyer == Address::ZERO {
        return Err(provider_err(format!("escrow {id} does not exist")));
    }

    let state = match t.state {
        0 => EscrowState::Created,
        1 => EscrowState::Funded,
        2 => EscrowState::Shipped,
        3 => EscrowState::Completed,
        4 => EscrowState::Disputed,
        5 => EscrowState::Refunded,
        other => return Err(provider_err(format!("unknown escrow state {other}"))),
    };
    let deadline = |d: U256| (!d.is_zero()).then(|| d.saturating_to::<u64>());

    Ok(EscrowInfo {
        id,
        buyer: t.buyer.to_string(),
        seller: (t.seller != Address::ZERO).then(|| t.seller.to_string()),
        amount_wei: t.amount.to_string(),
        state,
        ship_deadline: deadline(t.shipDeadline),
        confirm_deadline: deadline(t.confirmDeadline),
    })
}

/// Signs with the mnemonic's first account, sends, and waits for the receipt.
/// Reverts surface from gas estimation with the contract's reason string.
async fn send_tx(
    mnemonic: &str,
    rpc_url: &str,
    tx: TransactionRequest,
) -> Result<TransactionReceipt, AlloyError> {
    let signer = MnemonicBuilder::<English>::default()
        .phrase(mnemonic)
        .build()
        .map_err(|e| AlloyError::MnemonicError {
            message: e.to_string(),
        })?;
    let provider = ProviderBuilder::new()
        .wallet(signer)
        .connect_http(parse_url(rpc_url)?);

    let receipt = provider
        .send_transaction(tx)
        .await
        .map_err(provider_err)?
        .get_receipt()
        .await
        .map_err(provider_err)?;
    if !receipt.status() {
        return Err(provider_err(format!(
            "transaction {} reverted",
            receipt.transaction_hash
        )));
    }
    Ok(receipt)
}

/// Same per-call runtime pattern as the rest of the crate: the UniFFI future
/// is polled by Swift, not tokio, so reqwest needs a runtime of its own.
fn block_on<T>(fut: impl Future<Output = Result<T, AlloyError>>) -> Result<T, AlloyError> {
    tokio::runtime::Runtime::new().map_err(provider_err)?.block_on(fut)
}

fn parse_url(url: &str) -> Result<Url, AlloyError> {
    url.parse::<Url>().map_err(|e| AlloyError::UrlError {
        message: e.to_string(),
    })
}

fn parse_address(address: &str) -> Result<Address, AlloyError> {
    Address::from_str(address).map_err(|e| AlloyError::AddressError {
        message: e.to_string(),
    })
}

fn provider_err(e: impl std::fmt::Display) -> AlloyError {
    AlloyError::ProviderError {
        message: e.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};

    // Anvil's default mnemonic: account 0 is funded (buyer). The seller uses a
    // separate mnemonic and gets gas money from the buyer first.
    const BUYER: &str = "test test test test test test test test test test test junk";
    const SELLER: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
    const RPC: &str = "http://127.0.0.1:8545";

    /// Exported fns finish on the first poll (they block_on inside), and they
    /// can't run under #[tokio::test] because that would nest runtimes.
    fn run<T>(fut: impl Future<Output = T>) -> T {
        match pin!(fut).poll(&mut Context::from_waker(Waker::noop())) {
            Poll::Ready(v) => v,
            Poll::Pending => panic!("future did not complete synchronously"),
        }
    }

    /// Needs `anvil` running with Escrow deployed (contracts/script/Deploy.s.sol).
    /// Run: ESCROW_CONTRACT=0x... cargo test -- --ignored
    #[test]
    #[ignore]
    fn full_flow_on_anvil() {
        let contract = std::env::var("ESCROW_CONTRACT")
            .unwrap_or_else(|_| "0x5FbDB2315678afecb367f032d93F642f64180aa3".into());
        let c = contract.as_str();
        let seller = run(crate::derive_address_from_mnemonic(SELLER)).unwrap();
        let gas_money = "1000000000000000001"; // 1 ETH + 1 wei: checks the amount is exact
        let before: U256 = run(get_balance(RPC, &seller)).unwrap().parse().unwrap();
        run(transfer_eth(BUYER, RPC, &seller, gas_money)).unwrap();
        let after: U256 = run(get_balance(RPC, &seller)).unwrap().parse().unwrap();
        assert_eq!(after - before, U256::from_str(gas_money).unwrap());

        let amount = "500000000000000000"; // 0.5 ETH
        let id = run(create_escrow(BUYER, RPC, c, amount)).unwrap();
        let info = run(get_escrow(RPC, c, id)).unwrap();
        assert_eq!(info.state, EscrowState::Funded);
        assert_eq!(info.amount_wei, amount);
        assert!(info.seller.is_none());
        assert!(info.ship_deadline.is_some());

        run(escrow_action(SELLER, RPC, c, EscrowAction::Join, id)).unwrap();
        run(escrow_action(SELLER, RPC, c, EscrowAction::MarkShipped, id)).unwrap();

        let err = run(escrow_action(SELLER, RPC, c, EscrowAction::ConfirmReceipt, id)).unwrap_err();
        assert!(err.to_string().contains("only buyer can confirm"), "{err}");

        run(escrow_action(BUYER, RPC, c, EscrowAction::ConfirmReceipt, id)).unwrap();
        let info = run(get_escrow(RPC, c, id)).unwrap();
        assert_eq!(info.state, EscrowState::Completed);
        assert_eq!(info.seller.as_deref(), Some(seller.as_str()));

        let owed: U256 = run(pending_withdrawal(RPC, c, &seller)).unwrap().parse().unwrap();
        assert!(owed >= U256::from_str(amount).unwrap());
        let before: U256 = run(get_balance(RPC, &seller)).unwrap().parse().unwrap();
        run(withdraw(SELLER, RPC, c)).unwrap();
        let after: U256 = run(get_balance(RPC, &seller)).unwrap().parse().unwrap();
        assert!(after > before);
        assert_eq!(run(pending_withdrawal(RPC, c, &seller)).unwrap(), "0");

        let mine = run(list_escrows_for(RPC, c, &seller)).unwrap();
        assert!(mine.iter().any(|e| e.id == id && e.state == EscrowState::Completed));

        assert!(run(get_escrow(RPC, c, u64::MAX)).is_err());
    }
}
