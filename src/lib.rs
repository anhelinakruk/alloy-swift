use alloy::providers::{Provider, ProviderBuilder};
use alloy::sol;
use alloy::transports::http::reqwest::Url;
use alloy::{
    primitives::{Address, U256},
    rpc::types::TransactionRequest,
    signers::Signer,
};
use alloy_network::TransactionBuilder;
use alloy_signer_local::{coins_bip39::English, MnemonicBuilder};
use coins_bip39::{Entropy, Mnemonic};
use std::str::FromStr;

uniffi::setup_scaffolding!();

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum AlloyError {
    #[error("Mnemonic error: {message}")]
    MnemonicError { message: String },
    #[error("Address parsing error: {message}")]
    AddressError { message: String },
    #[error("URL parsing error: {message}")]
    UrlError { message: String },
    #[error("Provider error: {message}")]
    ProviderError { message: String },
}

#[uniffi::export]
pub async fn derive_address_from_mnemonic(mnemonic: &str) -> Result<String, AlloyError> {
    let mnemonic_signer = MnemonicBuilder::<English>::default()
        .phrase(mnemonic)
        .build()
        .map_err(|e| AlloyError::MnemonicError {
            message: e.to_string(),
        })?;

    let address = mnemonic_signer.address();
    println!("Derived address: {:?}", address.to_string());

    Ok(address.to_string())
}

#[uniffi::export]
pub async fn generate_mnemonic(bytes: &[u8]) -> Result<String, AlloyError> {
    let entropy = Entropy::from_slice(bytes).map_err(|e| AlloyError::MnemonicError {
        message: e.to_string(),
    })?;

    let mnemonic: Mnemonic<English> = Mnemonic::new_from_entropy(entropy);
    let phrase = mnemonic.to_phrase();

    Ok(phrase)
}

#[uniffi::export]
pub async fn sign_message(mnemonic: &str, message: &str) -> Result<String, AlloyError> {
    let mnemonic_signer = MnemonicBuilder::<English>::default()
        .phrase(mnemonic)
        .build()
        .map_err(|e| AlloyError::MnemonicError {
            message: e.to_string(),
        })?;

    let signature = mnemonic_signer
        .sign_message(message.as_bytes())
        .await
        .map_err(|e| AlloyError::MnemonicError {
            message: e.to_string(),
        })?;
    let signature_hex = format!("0x{}", hex::encode(signature.as_bytes()));
    Ok(signature_hex)
}

sol! {
    event Transfer(address indexed from, address indexed to, uint256 value);

    interface IERC20 {
        function transfer(address to, uint256 amount) external returns (bool);
    }
}

sol!(
    #[sol(rpc)]
    Erc20Contract,
    "src/abi/erc20.json"
);

#[uniffi::export]
pub async fn send_transaction(
    mnemonic: &str,
    to: &str,
    amount: i64,
    token_address: &str,
    network_rpc_url: &str,
    token_decimals: u8,
    crypto_precision: u8,
) -> Result<String, AlloyError> {
    let rt = tokio::runtime::Runtime::new().map_err(|e| AlloyError::ProviderError {
        message: e.to_string(),
    })?;
    rt.block_on(async {
        let mnemonic_signer = MnemonicBuilder::<English>::default()
            .phrase(mnemonic)
            .build()
            .map_err(|e| AlloyError::MnemonicError {
                message: e.to_string(),
            })?;

        let provider = ProviderBuilder::new().wallet(mnemonic_signer).connect_http(
            network_rpc_url
                .parse::<Url>()
                .map_err(|e| AlloyError::UrlError {
                    message: e.to_string(),
                })?,
        );

        let token_address =
            Address::from_str(token_address).map_err(|e| AlloyError::AddressError {
                message: e.to_string(),
            })?;

        let to_address = Address::from_str(to).map_err(|e| AlloyError::AddressError {
            message: e.to_string(),
        })?;

        let amount = U256::from(
            amount as i128 * 10i128.pow(token_decimals as u32 - crypto_precision as u32),
        );

        let call = IERC20::transferCall {
            to: to_address,
            amount,
        };

        println!("Token address: {:?}", token_address);
        println!("To address: {:?}", to_address);
        println!("Amount: {:?}", amount);

        let tx = TransactionRequest::default()
            .with_to(token_address)
            .with_value(U256::from(0))
            .with_call(&call);

        let gas_estimate =
            provider
                .estimate_gas(tx.clone())
                .await
                .map_err(|e| AlloyError::ProviderError {
                    message: e.to_string(),
                })?;

        println!("Transaction: {:?}", tx);
        println!("Gas estimate: {:?}", gas_estimate);

        let tx_hash = provider
            .send_transaction(tx.with_gas_limit(gas_estimate))
            .await
            .map_err(|e| AlloyError::ProviderError {
                message: e.to_string(),
            })?
            .watch()
            .await
            .map_err(|e| AlloyError::ProviderError {
                message: e.to_string(),
            })?;

        println!("Transaction sent: {:?}", tx_hash);

        Ok(tx_hash.to_string())
    })
}

#[uniffi::export]
pub async fn send_eth_transaction(
    mnemonic: &str,
    to: &str,
    amount_eth: &str,
    network_rpc_url: &str,
    chain_id: u64,
) -> Result<String, AlloyError> {
    let rt = tokio::runtime::Runtime::new().map_err(|e| AlloyError::ProviderError {
        message: e.to_string(),
    })?;

    rt.block_on(async {
        let mnemonic_signer = MnemonicBuilder::<English>::default()
            .phrase(mnemonic)
            .build()
            .map_err(|e| AlloyError::MnemonicError {
                message: e.to_string(),
            })?;

        let provider = ProviderBuilder::new()
            .wallet(mnemonic_signer)
            .connect_http(network_rpc_url.parse::<Url>()
                .map_err(|e| AlloyError::UrlError { message: e.to_string() })?
            );

        let to_address = Address::from_str(to)
            .map_err(|e| AlloyError::AddressError { message: e.to_string() })?;

        let amount_wei = {
            let amount_f64: f64 = amount_eth.parse()
                .map_err(|e| AlloyError::ProviderError {
                    message: format!("Invalid amount format: {}", e)
                })?;

            let wei_f64 = amount_f64 * 1_000_000_000_000_000_000.0;
            U256::from(wei_f64 as u128)
        };

        let tx = TransactionRequest::default()
            .with_to(to_address)
            .with_value(amount_wei)
            .with_chain_id(chain_id);

        let gas_estimate = provider.estimate_gas(tx.clone()).await
            .map_err(|e| AlloyError::ProviderError { message: e.to_string() })?;

        let tx_hash = provider
            .send_transaction(tx.with_gas_limit(gas_estimate))
            .await
            .map_err(|e| AlloyError::ProviderError { message: e.to_string() })?
            .watch()
            .await
            .map_err(|e| AlloyError::ProviderError { message: e.to_string() })?;

        Ok(tx_hash.to_string())
    })
}

#[uniffi::export]
pub fn is_valid_address(address: &str) -> bool {
    Address::from_str(address).is_ok()
}


#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_generate_mnemonic() {
        let result = generate_mnemonic(&[0; 32]).await;
        assert!(result.is_ok());

        let mnemonic = result.unwrap();
        let words: Vec<&str> = mnemonic.split_whitespace().collect();

        assert_eq!(words.len(), 12);
        for word in words {
            assert!(!word.is_empty());
        }
    }

    #[tokio::test]
    async fn test_derive_address_from_mnemonic() {
        let test_mnemonic = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

        let result = derive_address_from_mnemonic(test_mnemonic).await;
        assert!(result.is_ok());

        let address = result.unwrap();
        assert!(address.starts_with("0x"));
        assert_eq!(address.len(), 42);
    }

    #[tokio::test]
    async fn test_derive_address_deterministic() {
        // Same mnemonic should always produce the same address
        let test_mnemonic = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

        let address1 = derive_address_from_mnemonic(test_mnemonic).await.unwrap();
        let address2 = derive_address_from_mnemonic(test_mnemonic).await.unwrap();

        assert_eq!(address1, address2);
    }

    #[tokio::test]
    async fn test_derive_address_from_invalid_mnemonic() {
        let invalid_mnemonic = "invalid mnemonic phrase that is not valid";

        let result = derive_address_from_mnemonic(invalid_mnemonic).await;
        assert!(result.is_err());

        match result.unwrap_err() {
            AlloyError::MnemonicError { .. } => {
                // Expected error type
            }
            _ => panic!("Expected MnemonicError"),
        }
    }

    #[tokio::test]
    async fn test_generate_and_derive_integration() {
        // Generate a mnemonic and then derive an address from it
        let mnemonic = generate_mnemonic(&[0; 32]).await.unwrap();
        let address = derive_address_from_mnemonic(&mnemonic).await.unwrap();

        // Address should be valid
        assert!(address.starts_with("0x"));
        assert_eq!(address.len(), 42);
    }
}
