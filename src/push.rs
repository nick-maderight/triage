//! Web Push to browsers and Home Screen apps with VAPID (RFC 8292) and aes128gcm payloads (RFC 8291).
//! Owner: server worker.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair};
use ring::{aead, agreement, hkdf};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

/// A browser's push subscription and the largest card priority number it wants to hear about.
#[derive(Debug, Clone, PartialEq)]
pub struct Subscription {
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
    pub max_priority: u8,
}

impl Subscription {
    /// Checks what a browser sent before it is stored. The error is the 400 message.
    pub fn checked(endpoint: String, p256dh: String, auth: String, max_priority: u8) -> Result<Subscription, &'static str> {
        let https = endpoint.len() <= 2048
            && endpoint
                .parse::<axum::http::Uri>()
                .is_ok_and(|uri| uri.scheme_str() == Some("https") && uri.host().is_some());
        if !https {
            return Err("endpoint must be an https URL");
        }
        if !decode(&p256dh).is_ok_and(|key| key.len() == 65 && key[0] == 4) {
            return Err("p256dh must be an uncompressed P-256 public key");
        }
        if !decode(&auth).is_ok_and(|secret| secret.len() == 16) {
            return Err("auth must be 16 bytes");
        }
        if !(1..=5).contains(&max_priority) {
            return Err("max_priority must be 1 to 5");
        }
        Ok(Subscription { endpoint, p256dh, auth, max_priority })
    }
}

pub enum Sent {
    Delivered,
    /// The push service no longer knows the subscription (404 or 410), so the caller deletes it.
    Gone,
    Failed(String),
}

pub const TEST_PAYLOAD: &[u8] = br#"{"title":"Triage test","body":"Notifications reach this device.","tag":"triage-test"}"#;

pub struct Push {
    key: EcdsaKeyPair,
    public_key: String,
    subject: String,
    agent: ureq::Agent,
    /// One signed token per push service origin, with its expiry in Unix seconds.
    tokens: Mutex<HashMap<String, (String, u64)>>,
}

impl Push {
    /// Loads the daemon's VAPID key from the database, creating it on first start.
    pub fn load(db: &crate::db::Db, allowed_hosts: &[String]) -> Result<Push, String> {
        let rng = SystemRandom::new();
        let candidate = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng)
            .map_err(|_| "could not create a VAPID key".to_owned())?;
        let pkcs8 = db.vapid_key(candidate.as_ref()).map_err(|error| error.to_string())?;
        let key = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &pkcs8, &rng)
            .map_err(|_| "the stored VAPID key is unreadable".to_owned())?;
        let public_key = URL_SAFE_NO_PAD.encode(key.public_key().as_ref());
        // Apple rejects a localhost contact, and an iPhone reaches the inbox only through an allowed host anyway.
        let subject = allowed_hosts
            .first()
            .map_or_else(|| "https://localhost".to_owned(), |host| format!("https://{host}"));
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(10)))
            .build()
            .into();
        Ok(Push { key, public_key, subject, agent, tokens: Mutex::new(HashMap::new()) })
    }

    /// The uncompressed P-256 public key, base64url, for `PushManager.subscribe`.
    pub fn public_key(&self) -> &str {
        &self.public_key
    }

    /// Tells every subscription whose level admits the card. Blocks, so call it off the async workers.
    pub fn notify(&self, db: &crate::db::Db, card: &crate::events::Card) {
        let subscriptions = match db.push_subscriptions() {
            Ok(subscriptions) => subscriptions,
            Err(error) => {
                eprintln!("triage: push skipped ({error})");
                return;
            }
        };
        let payload = card_payload(card);
        let urgency = if card.priority <= 1 { "high" } else { "normal" };
        for subscription in subscriptions.iter().filter(|subscription| card.priority <= subscription.max_priority) {
            match self.send(subscription, &payload, urgency) {
                Sent::Delivered => {}
                Sent::Gone => {
                    if let Err(error) = db.delete_push_subscription(&subscription.endpoint) {
                        eprintln!("triage: could not drop a gone push subscription ({error})");
                    }
                }
                Sent::Failed(error) => {
                    eprintln!("triage: push to {} failed ({error})", origin(&subscription.endpoint).unwrap_or_default())
                }
            }
        }
    }

    /// Encrypts `payload` for one subscription and posts it. Blocks.
    pub fn send(&self, subscription: &Subscription, payload: &[u8], urgency: &str) -> Sent {
        match self.post(subscription, payload, urgency) {
            Ok((200..=299, _)) => Sent::Delivered,
            Ok((404 | 410, _)) => Sent::Gone,
            Ok((status, text)) => Sent::Failed(format!("push service answered {status} {}", text.trim())),
            Err(error) => Sent::Failed(error),
        }
    }

    fn post(&self, subscription: &Subscription, payload: &[u8], urgency: &str) -> Result<(u16, String), String> {
        let ua_public = decode(&subscription.p256dh).map_err(|error| error.to_string())?;
        let auth = decode(&subscription.auth).map_err(|error| error.to_string())?;
        let rng = SystemRandom::new();
        let mut salt = [0; 16];
        rng.fill(&mut salt).map_err(|_| "no randomness for the salt".to_owned())?;
        let body = encrypt(&ua_public, &auth, payload, &salt, &rng).map_err(|_| "could not encrypt the payload".to_owned())?;
        let now_s = u64::try_from(crate::events::now_ms()).unwrap_or(0) / 1000;
        let authorization = self.authorization(&subscription.endpoint, now_s)?;
        let mut response = self
            .agent
            .post(subscription.endpoint.as_str())
            .header("TTL", "86400")
            .header("Urgency", urgency)
            .header("Content-Type", "application/octet-stream")
            .header("Content-Encoding", "aes128gcm")
            .header("Authorization", authorization.as_str())
            .send(&body[..])
            .map_err(|error| error.to_string())?;
        let status = response.status().as_u16();
        let text = response.body_mut().with_config().limit(1024).read_to_string().unwrap_or_default();
        Ok((status, text))
    }

    /// `vapid t=<JWT>, k=<public key>` for the endpoint's push service.
    fn authorization(&self, endpoint: &str, now_s: u64) -> Result<String, String> {
        let audience = origin(endpoint).ok_or_else(|| "the endpoint has no origin".to_owned())?;
        let mut tokens = self.tokens.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let token = match tokens.get(&audience) {
            // Apple asks for a new token no more than once an hour, so one is reused until its last hour.
            Some((token, exp)) if *exp > now_s + 3600 => token.clone(),
            _ => {
                let exp = now_s + 12 * 3600;
                let token = self.jwt(&audience, exp)?;
                tokens.insert(audience, (token.clone(), exp));
                token
            }
        };
        Ok(format!("vapid t={token}, k={}", self.public_key))
    }

    fn jwt(&self, audience: &str, exp: u64) -> Result<String, String> {
        let claims = serde_json::json!({"aud": audience, "exp": exp, "sub": self.subject});
        let input = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(br#"{"typ":"JWT","alg":"ES256"}"#),
            URL_SAFE_NO_PAD.encode(claims.to_string())
        );
        let signature = self
            .key
            .sign(&SystemRandom::new(), input.as_bytes())
            .map_err(|_| "could not sign the VAPID token".to_owned())?;
        Ok(format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature.as_ref())))
    }
}

/// `scheme://host[:port]` of an endpoint, the JWT audience.
fn origin(endpoint: &str) -> Option<String> {
    let uri: axum::http::Uri = endpoint.parse().ok()?;
    Some(format!("{}://{}", uri.scheme_str()?, uri.authority()?.as_str()))
}

/// Browsers send unpadded base64url keys; a padded one still decodes.
fn decode(text: &str) -> Result<Vec<u8>, base64::DecodeError> {
    URL_SAFE_NO_PAD.decode(text.trim_end_matches('='))
}

fn card_payload(card: &crate::events::Card) -> Vec<u8> {
    serde_json::json!({"title": clip(&card.title), "body": clip(&card.source.label), "tag": card.id})
        .to_string()
        .into_bytes()
}

/// Keeps a notification line short and the payload far under the 3993 bytes one record carries.
fn clip(text: &str) -> String {
    if text.chars().count() <= 120 {
        return text.to_owned();
    }
    text.chars().take(119).chain(['…']).collect()
}

struct Len(usize);

impl hkdf::KeyType for Len {
    fn len(&self) -> usize {
        self.0
    }
}

fn hkdf_expand<const N: usize>(prk: &hkdf::Prk, info: &[u8]) -> Result<[u8; N], ring::error::Unspecified> {
    let mut out = [0; N];
    prk.expand(&[info], Len(N))?.fill(&mut out)?;
    Ok(out)
}

/// RFC 8291 aes128gcm body: salt, record size 4096, the sender's public key, then one record.
pub(crate) fn encrypt(
    ua_public: &[u8],
    auth: &[u8],
    plaintext: &[u8],
    salt: &[u8; 16],
    rng: &dyn SecureRandom,
) -> Result<Vec<u8>, ring::error::Unspecified> {
    let private = agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, rng)?;
    let as_public = private.compute_public_key()?;
    let mut key_info = b"WebPush: info\0".to_vec();
    key_info.extend_from_slice(ua_public);
    key_info.extend_from_slice(as_public.as_ref());
    let peer = agreement::UnparsedPublicKey::new(&agreement::ECDH_P256, ua_public);
    let ikm: [u8; 32] = agreement::agree_ephemeral(private, &peer, |secret| {
        hkdf_expand(&hkdf::Salt::new(hkdf::HKDF_SHA256, auth).extract(secret), &key_info)
    })??;
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, salt).extract(&ikm);
    let cek: [u8; 16] = hkdf_expand(&prk, b"Content-Encoding: aes128gcm\0")?;
    let nonce: [u8; 12] = hkdf_expand(&prk, b"Content-Encoding: nonce\0")?;
    let mut record = plaintext.to_vec();
    record.push(2); // the last record's delimiter, with no padding after it
    aead::LessSafeKey::new(aead::UnboundKey::new(&aead::AES_128_GCM, &cek)?).seal_in_place_append_tag(
        aead::Nonce::assume_unique_for_key(nonce),
        aead::Aad::empty(),
        &mut record,
    )?;
    let mut body = salt.to_vec();
    body.extend_from_slice(&4096u32.to_be_bytes());
    body.push(65);
    body.extend_from_slice(as_public.as_ref());
    body.extend_from_slice(&record);
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use std::path::Path;

    fn bytes(text: &str) -> Vec<u8> {
        URL_SAFE_NO_PAD.decode(text).unwrap()
    }

    // ring's fixed-output RNG is the only way to pin the sender's ephemeral key to the RFC's.
    #[allow(deprecated)]
    #[test]
    fn encrypts_the_rfc_8291_example() {
        let body = encrypt(
            &bytes("BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4"),
            &bytes("BTBZMqHH6r4Tts7J_aSIgg"),
            b"When I grow up, I want to be a watermelon",
            &bytes("DGv6ra1nlYgDCS1FRnbzlw").try_into().unwrap(),
            &ring::test::rand::FixedSliceRandom { bytes: &bytes("yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw") },
        )
        .unwrap();
        // RFC 8291 Section 5 and Appendix A.
        assert_eq!(
            URL_SAFE_NO_PAD.encode(body),
            "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPTpK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN"
        );
    }

    #[test]
    fn vapid_header_signs_for_the_push_service_and_reuses_the_token() {
        let db = Db::open(Path::new(":memory:")).unwrap();
        let push = Push::load(&db, &["machine.example.ts.net".to_owned()]).unwrap();
        let first = push.authorization("https://web.push.apple.com/QGuQ", 1_000_000).unwrap();

        let (token, key) = first.strip_prefix("vapid t=").unwrap().split_once(", k=").unwrap();
        assert_eq!(key, push.public_key());
        let (signed, signature) = token.rsplit_once('.').unwrap();
        ring::signature::UnparsedPublicKey::new(&ring::signature::ECDSA_P256_SHA256_FIXED, bytes(key))
            .verify(signed.as_bytes(), &bytes(signature))
            .expect("the token must verify with the advertised key");
        let claims: serde_json::Value = serde_json::from_slice(&bytes(signed.split_once('.').unwrap().1)).unwrap();
        assert_eq!(
            claims,
            serde_json::json!({"aud": "https://web.push.apple.com", "exp": 1_043_200, "sub": "https://machine.example.ts.net"})
        );

        assert_eq!(push.authorization("https://web.push.apple.com/other", 1_039_599).unwrap(), first);
        assert_ne!(push.authorization("https://web.push.apple.com/QGuQ", 1_039_601).unwrap(), first);
    }

    #[test]
    fn the_vapid_key_survives_a_restart() {
        // A new key on every start would silently strand every stored subscription.
        let path = std::env::temp_dir().join(format!("triage-push-{}.sqlite", uuid::Uuid::new_v4()));
        let db = Db::open(&path).unwrap();
        let before = Push::load(&db, &[]).unwrap().public_key().to_owned();
        drop(db);
        let db = Db::open(&path).unwrap();
        assert_eq!(Push::load(&db, &[]).unwrap().public_key(), before);
    }
}
