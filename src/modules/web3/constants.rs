// web3 -- Permission names and module constants.

// Web3 / Blockchain module.
//
// Openness and traceability for Unified records, in the style of a
// public blockchain explorer: watched addresses, on-chain transaction
// history, tamper-evident anchoring of record hashes, a public
// transparency ledger, read-only wallet linking, and read-only token
// balances across EVM chains, Stellar, and Cardano.
//
// SECURITY -- READ FIRST. These rules are absolute and are enforced by
// the code below, not just by policy:
//
//   - The server NEVER stores a user private key and NEVER signs a
//     transaction with user funds. There is no private-key field,
//     column, or configuration key anywhere in this module or in its
//     migration. Linked wallets are READ-ONLY: the server can observe
//     them, never spend from them.
//   - Anchoring uses a DEDICATED SERVICE KEY whose scope is
//     anchoring-only. The key is accepted once, stored write-only
//     (encrypted, never returned, never logged), and is used only to
//     authenticate the server to the configured anchoring relay. The
//     relay -- never this module -- submits the on-chain transaction.
//   - Any configuration value that looks like a private key (a
//     64-hex-character secret, a `0x`-prefixed 64-hex secret, a
//     wallet-import-format string, or a PEM private-key block) is
//     rejected at validation time.
//   - Only the SHA-256 hash of a record is ever anchored on-chain.
//     Record contents stay off-chain at all times.
//   - Every provider endpoint is validated against an allowlist
//     (known chain hosts, suffix rules, and admin-configured extra
//     hosts). Plain `http` is accepted only for loopback hosts, IP
//     literals are rejected except loopback, and a localhost or
//     private endpoint is never enabled unless an admin explicitly
//     allowlists it -- that decision is emitted on the hook bus as an
//     audit event (`web3.network.private_endpoint_allowed`).
//   - Provider failures are mapped to the standard error envelope;
//     provider response bodies are never echoed to callers.
//
// Provider model: every chain is reached through the `ChainProvider`
// trait. `EvmRpcProvider` speaks generic JSON-RPC for any EVM chain,
// `StellarProvider` speaks the Horizon API, and `CardanoProvider`
// speaks Blockfrost- or Koios-style REST APIs. Networks are
// configuration, not code: adding a network never requires a code
// change. A `FakeChainProvider` exists for tests only (`#[cfg(test)]`).
//
// Honest limitations (see also the integrator report):
//   - No secp256k1 crate is present in `Cargo.toml`, so EVM message
//     verification cannot do `ecrecover` in-process. EVM verification
//     is delegated to an admin-configured, allowlisted verification
//     endpoint when one is set; without one, verification returns a
//     422 that says so plainly instead of pretending to verify.
//   - Stellar verification proves ownership by checking a transaction
//     from the address whose text memo carries the 28-character memo
//     token derived from the challenge nonce (a full 64-character
//     nonce cannot fit in a Stellar text memo, which holds at most
//     28 bytes).
//   - Cardano verification is lookup-only: it confirms the address
//     exists and is active, which does not by itself prove control.
//     That is reported honestly as `lookup_only`, never as verified.
//   - EVM address history needs an indexer endpoint (Etherscan-style
//     API) because plain JSON-RPC has no address transaction list.
//     Without an indexer, `get_transactions` on an EVM network
//     returns a 422 that says so.
//   - The in-memory DashMap store is authoritative; the migration
//     tables are persistence targets (same convention as the other
//     modules). The migration has never been executed against a live
//     PostgreSQL database.
//
// Events emitted on the hook bus: `web3.transaction.detected`,
// `web3.anchor.requested`, `web3.anchor.confirmed`,
// `web3.anchor.failed`, `web3.wallet.linked`, `web3.wallet.unlinked`,
// `web3.network.private_endpoint_allowed`.
// This module listens to no events.
//
// Capabilities: `web3:read`, `web3:write`, `web3:admin`, checked on
// every handler through the `role.check` hook (fails closed). The
// public ledger endpoint (`GET /api/v1/web3/public/ledger`) needs no
// caller headers, but it is default-deny: it answers 403 unless an
// admin enabled the public ledger in settings, and the API Management
// module's allowed-public-access controls are the outer gate for it.

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Capabilities enforced on the handlers.
pub const PERM_WEB3_READ: &str = "web3:read";

pub const PERM_WEB3_WRITE: &str = "web3:write";

pub const PERM_WEB3_ADMIN: &str = "web3:admin";

/// Confirmations after which an anchor counts as confirmed. The
/// provider-reported confirmation count is stored on every refresh;
/// this is the threshold for the `confirmed` transition and the
/// `web3.anchor.confirmed` event.
pub const REQUIRED_CONFIRMATIONS: i64 = 6;

/// Challenge nonces are valid for 15 minutes.
pub const CHALLENGE_TTL_SECS: i64 = 900;

/// Maximum transactions fetched from a provider in one poll per address.
pub const MAX_POLL_LIMIT: u32 = 50;

/// Per-request timeout for provider HTTP calls.
pub const PROVIDER_TIMEOUT_SECS: u64 = 15;

/// Nominal transaction size (bytes) used for the Cardano fee estimate:
/// `fee = min_fee_a * size + min_fee_b` from the protocol parameters.
pub const CARDANO_NOMINAL_TX_BYTES: u64 = 200;

/// Exact hosts a chain provider may be contacted on without any extra
/// configuration.
pub(crate) const KNOWN_CHAIN_HOSTS: [&str; 10] = [
    "horizon.stellar.org",
    "horizon-testnet.stellar.org",
    "api.blockfrost.io",
    "cardano-mainnet.blockfrost.io",
    "cardano-preprod.blockfrost.io",
    "cardano-preview.blockfrost.io",
    "api.koios.rest",
    "api.etherscan.io",
    "api-sepolia.etherscan.io",
    "api.bscscan.com",
];

/// Host suffixes that are always allowed: hosted-node and indexer
/// provider families.
pub(crate) const ALLOWED_CHAIN_HOST_SUFFIXES: [&str; 8] = [
    ".infura.io",
    ".alchemy.com",
    ".quiknode.pro",
    ".ankr.com",
    ".blockfrost.io",
    ".koios.rest",
    ".etherscan.io",
    ".bscscan.com",
];
