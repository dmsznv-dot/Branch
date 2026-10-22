use std::{fmt, str::FromStr};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use room_server::{InvitationToken, RoomError};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as DeError};
use sha2::{Digest, Sha256};
use shared_protocol::RoomId;
use subtle::ConstantTimeEq;
use url::Url;

const INVITATION_SCHEME: &str = "branch-watch";
const MAX_INVITATION_LENGTH: usize = 4096;
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct InvitationVerifier([u8; 32]);

impl InvitationVerifier {
    pub(crate) fn from_token(token: &InvitationToken) -> Self {
        Self(Sha256::digest(token.expose().as_bytes()).into())
    }

    fn parse(value: &str) -> Result<Self, InvitationError> {
        let bytes = URL_SAFE_NO_PAD
            .decode(value)
            .map_err(|_| InvitationError::InvalidFormat)?;
        if bytes.len() != 32 || URL_SAFE_NO_PAD.encode(&bytes) != value {
            return Err(InvitationError::InvalidFormat);
        }
        let mut digest = [0; 32];
        digest.copy_from_slice(&bytes);
        Ok(Self(digest))
    }

    pub fn expose(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.0)
    }

    pub(crate) fn matches(self, other: Self) -> bool {
        bool::from(self.0.ct_eq(&other.0))
    }
}

impl fmt::Debug for InvitationVerifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("InvitationVerifier([REDACTED])")
    }
}

impl Serialize for InvitationVerifier {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.expose())
    }
}

impl<'de> Deserialize<'de> for InvitationVerifier {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(D::Error::custom)
    }
}

#[derive(Clone)]
pub struct JoinInvitation {
    room_id: RoomId,
    signaling_url: String,
    capability: InvitationToken,
    certificate: Option<Vec<u8>>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InvitationPayload {
    version: u8,
    room_id: RoomId,
    signaling_url: String,
    capability: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    certificate: Option<Vec<u8>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvitationError {
    TooLarge,
    InvalidFormat,
    InvalidSignalingUrl,
}

impl fmt::Display for InvitationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::TooLarge => "invitation is too large",
            Self::InvalidFormat => "invalid invitation",
            Self::InvalidSignalingUrl => "invalid signaling address",
        })
    }
}
impl std::error::Error for InvitationError {}

impl JoinInvitation {
    pub fn new(
        room_id: RoomId,
        signaling_url: impl Into<String>,
        capability: InvitationToken,
    ) -> Result<Self, InvitationError> {
        let signaling_url = signaling_url.into();
        validate_signaling_url(&signaling_url)?;
        Ok(Self {
            room_id,
            signaling_url,
            capability,
            certificate: None,
        })
    }

    pub fn new_pinned(
        room_id: RoomId,
        signaling_url: impl Into<String>,
        capability: InvitationToken,
        certificate: Vec<u8>,
    ) -> Result<Self, InvitationError> {
        if certificate.is_empty() || certificate.len() > 16 * 1024 {
            return Err(InvitationError::InvalidFormat);
        }
        let mut invitation = Self::new(room_id, signaling_url, capability)?;
        invitation.certificate = Some(certificate);
        Ok(invitation)
    }

    pub fn room_id(&self) -> RoomId {
        self.room_id
    }
    pub fn signaling_url(&self) -> &str {
        &self.signaling_url
    }
    pub fn certificate(&self) -> Option<&[u8]> { self.certificate.as_deref() }
    pub fn verifier(&self) -> InvitationVerifier {
        InvitationVerifier::from_token(&self.capability)
    }
    pub fn capability(&self) -> &InvitationToken {
        &self.capability
    }

    /// Serialize a shareable custom-scheme URI. The secret remains in the URI
    /// fragment, which is not sent in HTTP requests or to the signaling service.
    pub fn to_share_string(&self) -> Result<String, InvitationError> {
        let payload = InvitationPayload {
            version: 1,
            room_id: self.room_id,
            signaling_url: self.signaling_url.clone(),
            capability: self.capability.expose().to_owned(),
            certificate: self.certificate.clone(),
        };
        let json = serde_json::to_vec(&payload).map_err(|_| InvitationError::InvalidFormat)?;
        let encoded = URL_SAFE_NO_PAD.encode(json);
        let share_string = format!("{INVITATION_SCHEME}://join/#{encoded}");
        if share_string.len() > MAX_INVITATION_LENGTH {
            return Err(InvitationError::TooLarge);
        }
        Ok(share_string)
    }

    pub fn parse_share_string(value: &str) -> Result<Self, InvitationError> {
        if value.len() > MAX_INVITATION_LENGTH {
            return Err(InvitationError::TooLarge);
        }
        let parsed = Url::parse(value).map_err(|_| InvitationError::InvalidFormat)?;
        if parsed.scheme() != INVITATION_SCHEME
            || parsed.host_str() != Some("join")
            || parsed.path() != "/"
            || parsed.username() != ""
            || parsed.password().is_some()
            || parsed.query().is_some()
        {
            return Err(InvitationError::InvalidFormat);
        }
        let fragment = parsed.fragment().ok_or(InvitationError::InvalidFormat)?;
        let bytes = URL_SAFE_NO_PAD
            .decode(fragment)
            .map_err(|_| InvitationError::InvalidFormat)?;
        if URL_SAFE_NO_PAD.encode(&bytes) != fragment {
            return Err(InvitationError::InvalidFormat);
        }
        let payload: InvitationPayload =
            serde_json::from_slice(&bytes).map_err(|_| InvitationError::InvalidFormat)?;
        if payload.version != 1 {
            return Err(InvitationError::InvalidFormat);
        }
        let capability = InvitationToken::parse(&payload.capability)
            .map_err(|_: RoomError| InvitationError::InvalidFormat)?;
        match payload.certificate {
            Some(certificate) => Self::new_pinned(payload.room_id, payload.signaling_url, capability, certificate),
            None => Self::new(payload.room_id, payload.signaling_url, capability),
        }
    }
}

impl fmt::Debug for JoinInvitation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JoinInvitation")
            .field("room_id", &self.room_id)
            .field("signaling_url", &self.signaling_url)
            .field("capability", &"[REDACTED]")
            .finish()
    }
}

impl FromStr for JoinInvitation {
    type Err = InvitationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse_share_string(value)
    }
}

fn validate_signaling_url(value: &str) -> Result<(), InvitationError> {
    let parsed = Url::parse(value).map_err(|_| InvitationError::InvalidSignalingUrl)?;
    if parsed.scheme() != "wss"
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || parsed.path() != "/v1/signal"
    {
        return Err(InvitationError::InvalidSignalingUrl);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use room_server::InvitationToken;
    use shared_protocol::RoomId;

    fn invitation() -> JoinInvitation {
        JoinInvitation::new(
            RoomId::new(),
            "wss://signal.example.org/v1/signal",
            InvitationToken::parse(&URL_SAFE_NO_PAD.encode([7; 32])).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn share_uri_round_trips_but_debug_redacts_capability() {
        let invite = invitation();
        let encoded = invite.to_share_string().unwrap();
        let parsed = JoinInvitation::parse_share_string(&encoded).unwrap();
        assert_eq!(parsed.room_id(), invite.room_id());
        assert_eq!(parsed.signaling_url(), invite.signaling_url());
        assert_eq!(parsed.capability().expose(), invite.capability().expose());
        assert!(!format!("{invite:?}").contains(invite.capability().expose()));
        assert!(format!("{invite:?}").contains("REDACTED"));
    }

    #[test]
    fn invitation_rejects_untrusted_or_noncanonical_endpoints() {
        let invite = invitation();
        let encoded = invite.to_share_string().unwrap();
        let fragment = encoded.split_once('#').unwrap().1;
        let json = URL_SAFE_NO_PAD.decode(fragment).unwrap();
        let mut payload: InvitationPayload = serde_json::from_slice(&json).unwrap();
        payload.signaling_url = "ws://signal.example.org".into();
        let tampered = format!(
            "{INVITATION_SCHEME}://join/#{}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap())
        );
        assert!(matches!(
            JoinInvitation::parse_share_string(&tampered),
            Err(InvitationError::InvalidSignalingUrl)
        ));
        assert!(JoinInvitation::parse_share_string("branch-watch://join/#not_base64!").is_err());
        assert!(
            JoinInvitation::parse_share_string(&"x".repeat(MAX_INVITATION_LENGTH + 1)).is_err()
        );
    }
}
