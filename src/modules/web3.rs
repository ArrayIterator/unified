// Web3 / Blockchain module: chain providers, watched addresses,
// on-chain anchors with a transparency ledger, wallet linking and
// balances. Split into one file per concern; this file only declares
// the submodules and re-exports the public API.

pub mod ai_capabilities;
pub mod constants;
pub mod events;
pub mod handlers_anchors;
pub mod handlers_networks;
pub mod handlers_settings;
pub mod handlers_wallets;
pub mod handlers_watched;
pub mod helpers;
pub mod module;
pub mod polling;
pub mod providers;
pub mod providers_cardano;
pub mod providers_evm;
pub mod providers_fake;
pub mod providers_stellar;
pub mod state;
#[cfg(test)]
mod tests;
pub mod types;
pub mod validation;

// Re-exported from constants.rs.
pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::constants::CARDANO_NOMINAL_TX_BYTES;
pub use self::constants::CHALLENGE_TTL_SECS;
pub use self::constants::MAX_POLL_LIMIT;
pub use self::constants::PERM_WEB3_ADMIN;
pub use self::constants::PERM_WEB3_READ;
pub use self::constants::PERM_WEB3_WRITE;
pub use self::constants::PROVIDER_TIMEOUT_SECS;
pub use self::constants::REQUIRED_CONFIRMATIONS;
// Re-exported from types.rs.
pub use self::types::AnchorRecord;
pub use self::types::BalanceSnapshot;
pub use self::types::ChainBalance;
pub use self::types::ChainKind;
pub use self::types::ChainNetwork;
pub use self::types::ChainTxRecord;
pub use self::types::CreateAnchorRequest;
pub use self::types::CreateNetworkRequest;
pub use self::types::CreateWatchedAddressRequest;
pub use self::types::FeeEstimate;
pub use self::types::LedgerEntry;
pub use self::types::LinkWalletRequest;
pub use self::types::NetworkStatus;
pub use self::types::ProviderTransaction;
pub use self::types::UpdateNetworkRequest;
pub use self::types::UpdateSettingsRequest;
pub use self::types::VerificationOutcome;
pub use self::types::VerifyAnchorRequest;
pub use self::types::VerifyWalletRequest;
pub use self::types::WalletLink;
pub use self::types::WatchedAddress;
pub use self::types::Web3Settings;
// Re-exported from state.rs.
pub use self::state::Web3State;
// Re-exported from validation.rs.
pub use self::validation::validate_chain_endpoint_url;
// Re-exported from providers.rs.
pub use self::providers::ChainProvider;
pub use self::providers::ChainProviderRegistry;
// Re-exported from providers_evm.rs.
pub use self::providers_evm::EvmRpcProvider;
// Re-exported from providers_stellar.rs.
pub use self::providers_stellar::StellarProvider;
// Re-exported from providers_cardano.rs.
pub use self::providers_cardano::CardanoProvider;
// Re-exported from providers_fake.rs.
#[cfg(test)]
pub use self::providers_fake::FakeChainProvider;
// Re-exported from polling.rs.
pub use self::polling::poll_watched_addresses;
// Re-exported from module.rs.
pub use self::module::Web3Module;
pub use self::module::register;
