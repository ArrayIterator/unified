// web3 -- Unit tests.

use super::helpers::*;
use super::providers::*;
use super::providers_fake::*;
use super::types::*;
use super::validation::*;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_to_decimal_converts_wei_values() {
        assert_eq!(hex_to_decimal("0x0").unwrap(), "0");
        assert_eq!(hex_to_decimal("0xff").unwrap(), "255");
        assert_eq!(
            hex_to_decimal("0xde0b6b3a7640000").unwrap(),
            "1000000000000000000"
        );
        assert_eq!(hex_to_decimal("0xzz"), None);
    }

    #[test]
    fn decimal_shift_scales_chain_amounts() {
        assert_eq!(decimal_shift("1000000000000000000", 18).unwrap(), "1");
        assert_eq!(decimal_shift("1500000", 6).unwrap(), "1.5");
        // Full precision is kept: no truncation to 4 fractional digits.
        assert_eq!(decimal_shift("123456789", 6).unwrap(), "123.456789");
        assert_eq!(decimal_shift("5", 6).unwrap(), "0.000005");
        assert_eq!(decimal_shift("0", 18).unwrap(), "0");
        assert_eq!(decimal_shift("12x", 6), None);
    }

    #[test]
    fn address_validation_is_kind_specific() {
        assert!(
            validate_address(ChainKind::Evm, "0x0000000000000000000000000000000000000000").is_ok()
        );
        assert!(validate_address(ChainKind::Evm, "0x123").is_err());
        // 55 characters: one short of a valid Stellar address.
        assert!(
            validate_address(
                ChainKind::Stellar,
                "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
            )
            .is_err()
        );
        // Lowercase characters are not valid Stellar base32 either.
        assert!(
            validate_address(
                ChainKind::Stellar,
                "Gaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            )
            .is_err()
        );
        // A 56-character G-prefixed all-A string IS a valid Stellar
        // address shape (base32), so it must not be rejected.
        assert!(
            validate_address(
                ChainKind::Stellar,
                "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
            )
            .is_ok()
        );
        assert!(
            validate_address(
                ChainKind::Stellar,
                "GBRPYHIL2CI3FNQ4BXLFMNDLFJUNPU2HY3ZMFSHONUCEOASW7QC7OX2H"
            )
            .is_ok()
        );
        assert!(validate_address(
            ChainKind::Cardano,
            "addr1qx2fxv2umyhttkxyxp8x0dlpdt3k6cwng5pxj3jhsydzr3jcu5d8ps7zex2k2xt3uqxgjqnnj83ws8lhrn648jjxtw3sshm7a"
        )
        .is_ok());
    }

    #[test]
    fn private_key_like_values_are_rejected() {
        assert!(looks_like_private_key(&"a".repeat(64)));
        assert!(looks_like_private_key(&format!("0x{}", "b".repeat(64))));
        assert!(looks_like_private_key(
            "-----BEGIN EC PRIVATE KEY-----\nAAAA\n-----END EC PRIVATE KEY-----"
        ));
        assert!(!looks_like_private_key("blockfrost-api-key-123"));
        assert!(!looks_like_private_key(""));
        assert!(reject_private_key_like(&"c".repeat(64), "api_key").is_err());
    }

    #[test]
    fn private_endpoints_need_an_explicit_admin_allowlist() {
        let result = validate_chain_endpoint_url(
            "http://localhost:8545",
            &[],
            false,
            &["localhost".to_string()],
        );
        assert!(result.is_err());
        let allowed = validate_chain_endpoint_url(
            "http://localhost:8545",
            &[],
            true,
            &["localhost".to_string()],
        );
        assert!(allowed.is_ok());
        let public = validate_chain_endpoint_url("https://horizon.stellar.org", &[], false, &[]);
        assert!(public.is_ok());
        let unknown = validate_chain_endpoint_url("https://example.com/rpc", &[], false, &[]);
        assert!(unknown.is_err());
    }

    #[test]
    fn secret_round_trip_never_exposes_plaintext() {
        let encrypted = encrypt_secret("test-secret", "provider-api-key");
        assert!(!encrypted.contains("provider-api-key"));
        assert_eq!(
            decrypt_secret("test-secret", &encrypted).unwrap(),
            "provider-api-key"
        );
        assert!(decrypt_secret("wrong-secret", &encrypted).is_err());
    }

    #[tokio::test]
    async fn fake_provider_reports_balances_and_transactions() {
        let provider = FakeChainProvider::new("fake-net", ChainKind::Evm);
        provider.set_balance("0xabc", "123000000");
        let balance = provider.get_balance("0xabc", None).await.unwrap();
        assert_eq!(balance.amount_raw, "123000000");
        provider.add_transaction(ProviderTransaction {
            tx_hash: "0xhash".to_string(),
            from_address: "0xabc".to_string(),
            to_address: "0xdef".to_string(),
            amount_raw: "100".to_string(),
            asset: "FAKE".to_string(),
            status: "confirmed".to_string(),
            confirmations: 3,
            block_number: Some(42),
            occurred_at: None,
        });
        let found = provider.get_transactions("0xabc", 10).await.unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].tx_hash, "0xhash");
    }
}
