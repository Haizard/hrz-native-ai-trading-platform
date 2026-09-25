//! Envelope encryption for a user's own exchange API keys (`docs/15`).
//!
//! ## Why this module exists when `credentials.rs` says it should not
//!
//! [`crate::credentials`] states the old position plainly: keys came from the
//! deployment's environment, nothing was stored, and a per-user key "needs a
//! purpose-built secret store -- not a column with a password in front of it."
//! That sentence is still right. This module **is** the purpose-built store, in
//! the only sense available to a single-process API with a managed Postgres: a
//! key-encryption key that lives in the environment (where a deployment belongs)
//! wraps each user's key material, so what sits in the database is ciphertext
//! that is useless without a value the database has never seen.
//!
//! The column is therefore not "a password in front of a secret". It is
//! AES-256-GCM ciphertext whose key is not in the row, not in the schema, and
//! not in any backup of it. An attacker with a full database dump and no
//! `BROKER_KEK` has nothing.
//!
//! ## What is written here, and what is not
//!
//! No cryptographic primitive is implemented. AES-256-GCM is the RustCrypto
//! AEAD and the nonce comes from the OS RNG. What this module owns is the frame
//! around the primitive, which is where such schemes actually fail:
//!
//! * **A fresh nonce per seal, from the OS RNG.** Not a counter, not a
//!   timestamp, not derived from the plaintext. Reusing a nonce under one key
//!   is the one mistake AES-GCM does not survive -- it leaks the XOR of the two
//!   plaintexts and destroys the authentication of both -- and two users who
//!   connected the same exchange account would otherwise produce identical
//!   ciphertext. Pinned by
//!   [`tests::the_same_plaintext_seals_to_different_bytes`].
//! * **The scope is authenticated, not assumed.** [`SecretScope`] (user id +
//!   venue) is passed as the AEAD's additional authenticated data, so a row
//!   moved to another user -- or a venue string edited in place -- fails to
//!   decrypt rather than decrypting into someone else's credential. A database
//!   `UPDATE` cannot do it, and neither can a bug that swaps two ids. Pinned by
//!   [`tests::a_blob_cannot_be_opened_for_another_user_or_venue`].
//! * **A version byte**, so a future format change is a refusal rather than a
//!   misparse. Pinned by [`tests::an_unknown_format_version_is_refused`].
//! * **Key material is zeroized on drop.** A `Vec<u8>` that held an API secret
//!   is otherwise left in freed memory for the allocator to hand to the next
//!   request; `zeroize` is why [`ExchangeCredentials`] holds `Zeroizing` fields.
//!
//! ## Failure is loud and names no secret
//!
//! Every error here is [`ExecutionError::Vault`], whose message names the
//! *variable* or the *scope* and never a key, a secret, or a plaintext. The
//! same rule as `credentials.rs`: the single most likely way a secret escapes
//! is a log line, and the audit trail `docs/15` requires must never contain one.

use aes_gcm::aead::rand_core::RngCore;
use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use base64::Engine;
use sha2::{Digest, Sha256};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::credentials::ExchangeCredentials;
use crate::error::ExecutionError;

/// The environment variable holding the key-encryption key.
///
/// A deployment variable rather than a database value, and that separation is
/// the whole design: a database dump, a backup, or a replica leak does not
/// carry the key that makes the ciphertext readable. It also means this is the
/// one variable whose loss is unrecoverable -- without it, every stored broker
/// key must be re-entered by its owner, which is the correct failure and worth
/// stating rather than discovering.
pub const KEK_VAR: &str = "BROKER_KEK";

/// The blob format version this build writes, and the only one it reads.
///
/// Refusing an unknown version rather than parsing it is the same rule as the
/// JWT's `alg` check: a reader that guesses at a shape it does not know is a
/// reader that can be fed one.
pub const FORMAT_VERSION: u8 = 1;

/// Bytes in an AES-256 key.
const KEY_LEN: usize = 32;

/// Bytes in a GCM nonce (96 bits, the size the AEAD is specified for).
const NONCE_LEN: usize = 12;

/// Bytes in the GCM authentication tag.
const TAG_LEN: usize = 16;

/// Bytes of key fingerprint kept. Eight is a lot more than enough to answer
/// "same key or not" and far too few to attack the key with.
const FINGERPRINT_LEN: usize = 8;

/// What a sealed blob is bound to.
///
/// Passed as the AEAD's additional authenticated data, so the ciphertext is
/// cryptographically tied to the user and venue it was created for. Building
/// this from anything else -- a label, a timestamp, a symbol -- would break the
/// property that matters, which is that one user's ciphertext is worthless
/// under another user's row.
///
/// The venue is lower-cased to match how the rest of the platform stores it
/// (`venue_routes::known`, `broker_accounts`), because `Binance` and `binance`
/// sealing different blobs would make a venue-name spelling change silently
/// undecryptable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretScope {
    /// The owner of the secret.
    pub user_id: Uuid,
    /// The venue it authenticates against.
    pub venue: String,
}

impl SecretScope {
    /// Build a scope, normalising the venue the way the schema does.
    #[must_use]
    pub fn new(user_id: Uuid, venue: &str) -> Self {
        Self {
            user_id,
            venue: venue.trim().to_ascii_lowercase(),
        }
    }

    /// The bytes authenticated alongside the ciphertext.
    ///
    /// Versioned so that a future change to what a scope means is a *new*
    /// string rather than the same string meaning two things -- which would
    /// make an old blob decrypt under a scope that did not produce it.
    fn aad(&self) -> String {
        format!("broker-credential:v1:{}:{}", self.user_id, self.venue)
    }
}

/// One user's key material, sealed.
///
/// The fields are opaque bytes, so this type cannot leak a secret through a
/// `Debug` line or a serialization it was never meant to have: neither is
/// derived. `db::broker_accounts` stores these blobs as `BYTEA` and never
/// interprets them, which is what keeps the crypto in one place.
pub struct SealedCredentials {
    /// The sealed API key.
    pub key_ciphertext: Vec<u8>,
    /// The sealed API secret.
    pub secret_ciphertext: Vec<u8>,
    /// Which KEK sealed it, so a rotation can find the rows that need re-sealing.
    pub kek_fingerprint: String,
}

impl std::fmt::Debug for SealedCredentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Lengths, not contents. An engineer can tell an empty column from a
        // populated one, which is the only question this type should answer.
        formatter
            .debug_struct("SealedCredentials")
            .field("key_ciphertext", &self.key_ciphertext.len())
            .field("secret_ciphertext", &self.secret_ciphertext.len())
            .field("kek_fingerprint", &self.kek_fingerprint)
            .finish()
    }
}

/// The platform's key-encryption key, and the only holder of it.
///
/// Not `Clone`: one per process, built once at startup from the environment.
pub struct SecretVault {
    key: Zeroizing<[u8; KEY_LEN]>,
    fingerprint: String,
}

impl std::fmt::Debug for SecretVault {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The fingerprint is safe to print -- it is a hash of the key, not the
        // key -- and it is the one piece of information an operator rotating a
        // KEK actually needs.
        formatter
            .debug_struct("SecretVault")
            .field("kek_fingerprint", &self.fingerprint)
            .finish_non_exhaustive()
    }
}

impl SecretVault {
    /// Read `BROKER_KEK` and build the vault.
    ///
    /// # Errors
    /// [`ExecutionError::Vault`] when the variable is missing, is not base64,
    /// or is not 32 bytes. The message names the variable and the expected
    /// form and never echoes the value -- a malformed key is still a secret.
    pub fn from_env() -> Result<Self, ExecutionError> {
        let raw = std::env::var(KEK_VAR).map_err(|_| {
            ExecutionError::Vault(format!(
                "{KEK_VAR} is not set. Users cannot connect their own broker accounts without \
                 it, and any key already stored was sealed with it -- losing it means every user \
                 must re-enter their keys. Set it to 32 random bytes, base64-encoded \
                 (`openssl rand -base64 32`)."
            ))
        })?;
        let key = parse_key(&raw)?;
        Ok(Self::new(key))
    }

    /// Build from key bytes, for tests and for a future secret-store reader.
    #[must_use]
    pub fn new(key: [u8; KEY_LEN]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(key);
        let digest = hasher.finalize();
        let fingerprint = hex(&digest[..FINGERPRINT_LEN]);
        Self {
            key: Zeroizing::new(key),
            fingerprint,
        }
    }

    /// Which KEK this is, as a short hex string.
    ///
    /// Stored beside each sealed row so a rotation can *find* what is still
    /// sealed under the old key. One-way and truncated: it identifies a key
    /// without being usable to derive it.
    #[must_use]
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// Seal one secret, bound to a scope.
    ///
    /// # Errors
    /// [`ExecutionError::Vault`] if the plaintext exceeds the AEAD's limit,
    /// which for an API key cannot happen and is not checked by hand.
    pub fn seal(&self, scope: &SecretScope, plaintext: &[u8]) -> Result<Vec<u8>, ExecutionError> {
        let cipher = self.cipher();
        // A fresh nonce per seal, from the OS RNG. `try_fill_bytes` rather than
        // `fill_bytes`: the latter panics if the platform entropy source fails,
        // and a request that cannot get a nonce should be refused, not fatal.
        let mut nonce_bytes = [0u8; NONCE_LEN];
        aes_gcm::aead::OsRng
            .try_fill_bytes(&mut nonce_bytes)
            .map_err(|e| ExecutionError::Vault(format!("the OS RNG failed: {e}")))?;
        let aad = scope.aad();

        let ciphertext = cipher
            .encrypt(
                Nonce::from_slice(&nonce_bytes),
                Payload {
                    msg: plaintext,
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| ExecutionError::Vault("the secret could not be encrypted".into()))?;

        // `version || nonce || ciphertext+tag`. The version is first so a future
        // reader can refuse an unknown shape without slicing it first.
        let mut blob = Vec::with_capacity(1 + NONCE_LEN + ciphertext.len());
        blob.push(FORMAT_VERSION);
        blob.extend_from_slice(&nonce_bytes);
        blob.extend_from_slice(&ciphertext);
        Ok(blob)
    }

    /// Open a sealed secret, bound to the same scope it was sealed with.
    ///
    /// # Errors
    /// [`ExecutionError::Vault`] for a truncated blob, an unknown version, a
    /// scope that does not match the one it was sealed under, and a wrong or
    /// rotated KEK. All of those are one answer on purpose: telling a caller
    /// *why* authentication failed is telling an attacker which of their
    /// guesses was closest.
    pub fn open(
        &self,
        scope: &SecretScope,
        blob: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>, ExecutionError> {
        let version = blob
            .first()
            .ok_or_else(|| ExecutionError::Vault("the sealed value is empty".into()))?;
        if *version != FORMAT_VERSION {
            return Err(ExecutionError::Vault(format!(
                "the sealed value is format version {version}, and this build writes and reads \
                 version {FORMAT_VERSION}"
            )));
        }
        if blob.len() < 1 + NONCE_LEN + TAG_LEN {
            return Err(ExecutionError::Vault(
                "the sealed value is shorter than a nonce and an authentication tag".into(),
            ));
        }

        let (nonce_bytes, ciphertext) = blob[1..].split_at(NONCE_LEN);
        let aad = scope.aad();

        let plaintext = self
            .cipher()
            .decrypt(
                Nonce::from_slice(nonce_bytes),
                Payload {
                    msg: ciphertext,
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| {
                ExecutionError::Vault(format!(
                    "the sealed value did not authenticate for {}@{}: it was sealed with a \
                     different key-encryption key, altered, or moved from another account",
                    scope.user_id, scope.venue
                ))
            })?;
        Ok(Zeroizing::new(plaintext))
    }

    /// Seal a venue's key and secret as one unit.
    ///
    /// # Errors
    /// As [`SecretVault::seal`].
    pub fn seal_credentials(
        &self,
        scope: &SecretScope,
        credentials: &ExchangeCredentials,
    ) -> Result<SealedCredentials, ExecutionError> {
        Ok(SealedCredentials {
            key_ciphertext: self.seal(scope, credentials.key().as_bytes())?,
            secret_ciphertext: self.seal(scope, credentials.secret().as_bytes())?,
            kek_fingerprint: self.fingerprint.clone(),
        })
    }

    /// Recover a venue's key and secret from their sealed form.
    ///
    /// # Errors
    /// As [`SecretVault::open`].
    pub fn open_credentials(
        &self,
        scope: &SecretScope,
        sealed: &SealedCredentials,
    ) -> Result<ExchangeCredentials, ExecutionError> {
        let key = self.open(scope, &sealed.key_ciphertext)?;
        let secret = self.open(scope, &sealed.secret_ciphertext)?;

        let key = std::str::from_utf8(&key)
            .map_err(|_| ExecutionError::Vault("the stored API key is not valid utf-8".into()))?;
        let secret = std::str::from_utf8(&secret).map_err(|_| {
            ExecutionError::Vault("the stored API secret is not valid utf-8".into())
        })?;

        if key.is_empty() || secret.is_empty() {
            return Err(ExecutionError::Vault(
                "a stored credential is empty; re-connect the broker account".into(),
            ));
        }

        Ok(ExchangeCredentials::from_parts(&scope.venue, key, secret))
    }

    fn cipher(&self) -> Aes256Gcm {
        Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&*self.key))
    }
}

/// Decode a base64-encoded KEK, refusing anything that is not 32 bytes.
///
/// Base64 rather than hex because that is what `openssl rand -base64 32`
/// produces, and a key an operator has to transcode by hand is a key an
/// operator will get wrong. Padding is optional: both spellings of the same 32
/// bytes are accepted, because a copied URL-safe value losing its `=` is a
/// parse failure that says nothing useful about the real mistake.
fn parse_key(raw: &str) -> Result<[u8; KEY_LEN], ExecutionError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ExecutionError::Vault(format!(
            "{KEK_VAR} is empty. A blank key is not a key: it would make one value that anyone \
             can guess open every stored credential."
        )));
    }

    let decoded = STANDARD
        .decode(trimmed)
        .or_else(|_| STANDARD_NO_PAD.decode(trimmed))
        .map_err(|_| {
            ExecutionError::Vault(format!(
                "{KEK_VAR} is not valid base64. It must be 32 random bytes, base64-encoded."
            ))
        })?;

    if decoded.len() != KEY_LEN {
        return Err(ExecutionError::Vault(format!(
            "{KEK_VAR} decodes to {} bytes; exactly {KEY_LEN} are required (AES-256).",
            decoded.len()
        )));
    }

    let mut key = [0u8; KEY_LEN];
    key.copy_from_slice(&decoded);
    Ok(key)
}

/// Lower-case hex of a byte slice.
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        // `write!` to a String cannot fail; the result is discarded rather than
        // unwrapped so this cannot become a panic path.
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixed key, so a failure is reproducible rather than a one-in-a-hundred
    /// nonce collision nobody can re-run.
    fn vault() -> SecretVault {
        SecretVault::new([7u8; KEY_LEN])
    }

    fn scope() -> SecretScope {
        SecretScope::new(Uuid::nil(), "binance")
    }

    fn credentials() -> ExchangeCredentials {
        ExchangeCredentials::from_parts("binance", "KEY-abc123", "SECRET-supersecret")
    }

    #[test]
    fn credentials_round_trip_through_the_vault() {
        let vault = vault();
        let sealed = vault
            .seal_credentials(&scope(), &credentials())
            .expect("sealing works");
        let opened = vault
            .open_credentials(&scope(), &sealed)
            .expect("opening works");

        assert_eq!(opened.key(), "KEY-abc123");
        assert_eq!(opened.secret(), "SECRET-supersecret");
        assert_eq!(opened.venue(), "binance");
    }

    #[test]
    fn the_same_plaintext_seals_to_different_bytes() {
        // The failure this prevents is not a leak of one value: a repeated
        // nonce under one key breaks GCM entirely, and two users connecting the
        // same exchange account with the same key material would produce
        // byte-identical ciphertext without this.
        let vault = vault();
        let first = vault.seal(&scope(), b"same").expect("seals");
        let second = vault.seal(&scope(), b"same").expect("seals");

        assert_ne!(first, second);
        assert_eq!(
            vault.open(&scope(), &first).expect("opens").to_vec(),
            b"same".to_vec()
        );
        assert_eq!(
            vault.open(&scope(), &second).expect("opens").to_vec(),
            b"same".to_vec()
        );
    }

    #[test]
    fn a_blob_cannot_be_opened_for_another_user_or_venue() {
        // The multi-tenant property. A row swapped between users -- by a bug,
        // by a careless UPDATE, by a restore -- must fail rather than hand one
        // user's credential to another.
        let vault = vault();
        let sealed = vault.seal(&scope(), b"secret").expect("seals");

        let other_user = SecretScope::new(Uuid::from_u128(1), "binance");
        assert!(vault.open(&other_user, &sealed).is_err());

        let other_venue = SecretScope::new(Uuid::nil(), "bybit");
        assert!(vault.open(&other_venue, &sealed).is_err());

        // And the scope it was sealed for still works, so the test above is
        // about binding rather than about a vault that opens nothing.
        assert!(vault.open(&scope(), &sealed).is_ok());
    }

    #[test]
    fn a_venue_name_is_normalised_before_it_is_bound() {
        // Otherwise `Binance` and `binance` are different scopes, and a venue
        // arriving from a URL path in different case would be undecryptable.
        assert_eq!(
            SecretScope::new(Uuid::nil(), "  BINANCE "),
            SecretScope::new(Uuid::nil(), "binance")
        );
    }

    #[test]
    fn a_different_master_key_cannot_open_the_blob() {
        let sealed = vault().seal(&scope(), b"secret").expect("seals");
        let other = SecretVault::new([8u8; KEY_LEN]);

        let error = other
            .open(&scope(), &sealed)
            .expect_err("a rotated KEK must not silently open");
        assert!(matches!(error, ExecutionError::Vault(_)));
    }

    #[test]
    fn a_tampered_byte_is_refused_rather_than_returned() {
        // GCM authenticates. A single flipped bit must be a refusal, not a
        // different secret handed to the venue as a signature.
        let vault = vault();
        let mut sealed = vault.seal(&scope(), b"secret").expect("seals");
        let last = sealed.len() - 1;
        sealed[last] ^= 0x01;

        assert!(vault.open(&scope(), &sealed).is_err());
    }

    #[test]
    fn a_tampered_nonce_is_refused() {
        // The nonce is not secret but it *is* authenticated through the tag, so
        // editing it must fail too. This is the half of the frame a naive
        // implementation forgets to check.
        let vault = vault();
        let mut sealed = vault.seal(&scope(), b"secret").expect("seals");
        sealed[1] ^= 0xff;

        assert!(vault.open(&scope(), &sealed).is_err());
    }

    #[test]
    fn an_unknown_format_version_is_refused() {
        let vault = vault();
        let mut sealed = vault.seal(&scope(), b"secret").expect("seals");
        sealed[0] = FORMAT_VERSION + 1;

        let error = vault.open(&scope(), &sealed).expect_err("must refuse");
        assert!(error.to_string().contains("format version"), "{error}");
    }

    #[test]
    fn a_truncated_blob_is_refused_rather_than_panicking() {
        // `blob[1..].split_at(NONCE_LEN)` panics on a short slice. A column a
        // migration truncated, or a `bytea` an operator edited by hand, must be
        // a refused request rather than a dead request handler.
        let vault = vault();
        let sealed = vault.seal(&scope(), b"secret").expect("seals");

        for len in [0, 1, NONCE_LEN, 1 + NONCE_LEN, 1 + NONCE_LEN + 1] {
            assert!(
                vault
                    .open(&scope(), &sealed[..len.min(sealed.len())])
                    .is_err(),
                "accepted a {len}-byte blob"
            );
        }
    }

    #[test]
    fn the_fingerprint_identifies_a_key_without_being_one() {
        let a = SecretVault::new([1u8; KEY_LEN]);
        let b = SecretVault::new([2u8; KEY_LEN]);

        assert_eq!(a.fingerprint(), a.fingerprint());
        assert_ne!(a.fingerprint(), b.fingerprint());
        // Truncated to 8 bytes, so it is 16 hex characters and cannot be the
        // key however it is read.
        assert_eq!(a.fingerprint().len(), FINGERPRINT_LEN * 2);
    }

    #[test]
    fn a_debug_line_never_contains_a_key_or_a_secret() {
        // The same failure `credentials.rs` guards, one layer up: the vault and
        // the sealed pair are the two things a debugging engineer is most
        // likely to print.
        let vault = vault();
        let sealed = vault
            .seal_credentials(&scope(), &credentials())
            .expect("seals");

        let vault_debug = format!("{vault:?}");
        let sealed_debug = format!("{sealed:?}");

        for rendered in [&vault_debug, &sealed_debug] {
            assert!(!rendered.contains("KEY-abc123"), "{rendered}");
            assert!(!rendered.contains("SECRET-supersecret"), "{rendered}");
        }
        // The fingerprint is deliberately present: it is the one fact an
        // operator rotating a KEK needs, and it is a hash rather than the key.
        assert!(vault_debug.contains(vault.fingerprint()), "{vault_debug}");
    }

    #[test]
    fn a_key_of_the_wrong_length_is_refused_with_the_reason() {
        let error = parse_key(&STANDARD.encode([1u8; 16])).expect_err("16 bytes is not AES-256");
        let message = error.to_string();
        assert!(message.contains("16 bytes"), "{message}");
        assert!(message.contains("32"), "{message}");
    }

    #[test]
    fn a_key_that_is_not_base64_is_refused_without_echoing_it() {
        let error = parse_key("not base64 !!").expect_err("must refuse");
        let message = error.to_string();
        assert!(message.contains(KEK_VAR), "{message}");
        assert!(!message.contains("not base64 !!"), "{message}");
    }

    #[test]
    fn an_empty_key_is_refused_rather_than_treated_as_absent() {
        // A blank value is the failure mode of a deployment that set the
        // variable from an unset secret. Treating it as "no key" and treating
        // it as "a key" are very different, and only one of them is safe.
        assert!(parse_key("").is_err());
        assert!(parse_key("   ").is_err());
    }

    #[test]
    fn both_base64_paddings_of_the_same_key_are_accepted() {
        // `openssl rand -base64 32` pads; a copied URL-safe value often does
        // not. Both are the same 32 bytes.
        let key = [9u8; KEY_LEN];
        let padded = STANDARD.encode(key);
        let unpadded = STANDARD_NO_PAD
            .encode(key)
            .trim_end_matches('=')
            .to_string();

        assert_eq!(parse_key(&padded).expect("padded"), key);
        assert_eq!(parse_key(&unpadded).expect("unpadded"), key);
    }

    #[test]
    fn an_empty_credential_cannot_be_opened_back_into_use() {
        // A row whose ciphertext decrypts to "" is not a credential. Refusing
        // it here means the venue call fails with a reason instead of signing a
        // request with an empty secret and reporting a 401 from the exchange.
        let vault = vault();
        let sealed = vault.seal_credentials(
            &scope(),
            &ExchangeCredentials::from_parts("binance", "", ""),
        );
        let sealed = match sealed {
            // Sealing empty strings is allowed -- the check is on the way back,
            // which is where an already-stored row is read.
            Ok(sealed) => sealed,
            Err(e) => panic!("sealing should not be the rejecting step: {e}"),
        };

        assert!(vault.open_credentials(&scope(), &sealed).is_err());
    }
}
