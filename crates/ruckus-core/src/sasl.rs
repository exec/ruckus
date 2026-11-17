//! Synchronous IRCv3 SASL PLAIN/EXTERNAL helpers.
//!
//! Adapted from Lurk `sasl/`, Copyright (c) 2026 exec, MIT (see LICENSE).
//! TLS gating, CAP negotiation and timeouts remain the client's duty. Never log
//! returned lines: base64 is not encryption. SCRAM is not implemented.
use base64::{Engine as _, engine::general_purpose::STANDARD};
use std::fmt;

/// Maximum raw response size.
pub const MAX_RESPONSE_BYTES: usize = 8192;

/// Input-independent errors; server descriptions and credentials are never included.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    /// Invalid or oversized credentials.
    InvalidCredentials,
    /// Configured mechanism was not advertised.
    NotOffered,
    /// Operation is invalid in this state.
    InvalidState,
    /// PLAIN/EXTERNAL only accept a single empty server prompt.
    UnexpectedChallenge,
    /// Authentication was rejected.
    Rejected,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidCredentials => "invalid SASL credentials",
            Self::NotOffered => "SASL mechanism not offered",
            Self::InvalidState => "invalid SASL conversation state",
            Self::UnexpectedChallenge => "unexpected SASL challenge",
            Self::Rejected => "SASL authentication rejected",
        })
    }
}
impl std::error::Error for Error {}

/// Configured credentials. Debug redacts identities and passwords.
/// Released buffers are not cryptographically zeroized.
pub struct Mechanism {
    name: &'static str,
    response: Vec<u8>,
}
impl fmt::Debug for Mechanism {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Mechanism")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}
impl Mechanism {
    /// RFC 4616 response; NUL and empty account/password are rejected.
    /// Account Unicode preparation, if required, remains caller policy.
    pub fn plain(authzid: &str, authcid: &str, password: &str) -> Result<Self, Error> {
        let parts = [authzid, authcid, password];
        if authcid.is_empty()
            || password.is_empty()
            || parts.iter().any(|s| s.contains('\0'))
            || parts.iter().map(|s| s.len()).sum::<usize>() > MAX_RESPONSE_BYTES - 2
        {
            return Err(Error::InvalidCredentials);
        }
        Ok(Self {
            name: "PLAIN",
            response: parts.join("\0").into_bytes(),
        })
    }
    /// EXTERNAL authorization identity; authentication uses the TLS certificate.
    pub fn external(authzid: &str) -> Result<Self, Error> {
        if authzid.contains('\0') || authzid.len() > MAX_RESPONSE_BYTES {
            return Err(Error::InvalidCredentials);
        }
        Ok(Self {
            name: "EXTERNAL",
            response: authzid.as_bytes().to_vec(),
        })
    }
    /// Wire mechanism name.
    pub fn name(&self) -> &'static str {
        self.name
    }
    /// Check a case-sensitive list. An empty list means unrestricted SASL 3.1.
    pub fn check_offered(&self, offered: &[&str]) -> Result<(), Error> {
        if offered.is_empty() || offered.contains(&self.name) {
            Ok(())
        } else {
            Err(Error::NotOffered)
        }
    }
    /// Encode this response without consuming it. Returned lines are secrets.
    pub fn lines(&self) -> Result<Vec<String>, Error> {
        encode(&self.response)
    }
}

/// Base64-encode a bounded response into AUTHENTICATE lines without CRLF.
/// Empty and exact multiples of 400 encoded bytes terminate with `+`.
/// Returned strings contain secrets and must not be logged.
pub fn encode(response: &[u8]) -> Result<Vec<String>, Error> {
    if response.len() > MAX_RESPONSE_BYTES {
        return Err(Error::InvalidCredentials);
    }
    let encoded = STANDARD.encode(response);
    let mut lines: Vec<_> = encoded
        .as_bytes()
        .chunks(400)
        .map(|chunk| format!("AUTHENTICATE {}", String::from_utf8_lossy(chunk)))
        .collect();
    if encoded.len().is_multiple_of(400) {
        lines.push("AUTHENTICATE +".into());
    }
    Ok(lines)
}
#[derive(Debug, Eq, PartialEq)]
enum State {
    New,
    Prompt,
    Result,
    Finished,
}

/// Response to one inbound message. Debug redacts outgoing payloads.
pub struct Step {
    /// Lines to send without CRLF, potentially containing encoded secrets.
    pub lines: Vec<String>,
    /// Server confirmed success with 903.
    pub success: bool,
    /// Terminal error; transmit `lines`, including any abort, first.
    pub error: Option<Error>,
}
impl fmt::Debug for Step {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Step")
            .field("line_count", &self.lines.len())
            .field("success", &self.success)
            .field("error", &self.error)
            .finish()
    }
}
/// Single-use exchange with no I/O or automatic mechanism fallback.
#[derive(Debug)]
pub struct Conversation {
    mechanism: Mechanism,
    state: State,
}
impl Conversation {
    /// Configure an exchange.
    pub fn new(mechanism: Mechanism) -> Self {
        Self {
            mechanism,
            state: State::New,
        }
    }
    /// Select mechanism; wait for the server prompt before sending credentials.
    pub fn begin(&mut self) -> Result<String, Error> {
        if self.state != State::New {
            return Err(Error::InvalidState);
        }
        self.state = State::Prompt;
        Ok(format!("AUTHENTICATE {}", self.mechanism.name))
    }
    /// Process a parsed command/parameters; unrelated commands are ignored.
    /// 900/908 are informational; 901/902/904–907 reject authentication.
    /// Nonempty, malformed, or repeated prompts abort immediately: these
    /// mechanisms do not accept challenges, so no accumulation is necessary.
    /// Server-provided error descriptions are never retained.
    pub fn receive(&mut self, command: &str, params: &[&str]) -> Step {
        let mut step = Step {
            lines: Vec::new(),
            success: false,
            error: None,
        };
        if self.state == State::New || self.state == State::Finished {
            step.error = Some(Error::InvalidState);
            return step;
        }
        match command {
            "AUTHENTICATE" => {
                if self.state != State::Prompt || params != ["+"] {
                    step.lines.push("AUTHENTICATE *".into());
                    step.error = Some(Error::UnexpectedChallenge);
                } else {
                    match self.mechanism.lines() {
                        Ok(lines) => step.lines = lines,
                        Err(error) => step.error = Some(error),
                    }
                    self.mechanism.response.clear();
                    self.state = State::Result;
                }
            }
            "903" if self.state == State::Result => step.success = true,
            "903" => step.error = Some(Error::InvalidState),
            "901" | "902" | "904" | "905" | "906" | "907" => step.error = Some(Error::Rejected),
            _ => {}
        }
        if step.success || step.error.is_some() {
            self.state = State::Finished;
            self.mechanism.response.clear();
        }
        step
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chunk_boundaries() {
        for size in [0, 1, 297, 298, 299, 300, 301, 600, MAX_RESPONSE_BYTES] {
            let bytes = vec![42; size];
            let lines = encode(&bytes).unwrap();
            let parts: Vec<_> = lines
                .iter()
                .map(|s| s.strip_prefix("AUTHENTICATE ").unwrap())
                .collect();
            assert!(parts.iter().all(|s| s.len() <= 400));
            assert_eq!(
                parts.last() == Some(&"+"),
                STANDARD.encode(&bytes).len() % 400 == 0
            );
            assert_eq!(
                STANDARD
                    .decode(parts.into_iter().filter(|s| *s != "+").collect::<String>())
                    .unwrap(),
                bytes
            );
        }
        assert!(encode(&vec![0; MAX_RESPONSE_BYTES + 1]).is_err());
    }
    #[test]
    fn exchange_and_redaction() {
        let m = Mechanism::plain("", "account", "secret").unwrap();
        assert!(!format!("{m:?}").contains("secret"));
        assert_eq!(m.check_offered(&["EXTERNAL"]), Err(Error::NotOffered));
        assert!(m.check_offered(&[]).is_ok());
        let mut c = Conversation::new(m);
        assert_eq!(c.begin().unwrap(), "AUTHENTICATE PLAIN");
        assert!(c.begin().is_err());
        let step = c.receive("AUTHENTICATE", &["+"]);
        assert_eq!(step.lines, encode(b"\0account\0secret").unwrap());
        assert!(!format!("{step:?}").contains("AUTHENTICATE"));
        for command in ["900", "908", "PING"] {
            assert!(!c.receive(command, &["secret"]).success);
        }
        assert!(c.receive("903", &[]).success);
        assert_eq!(c.receive("903", &[]).error, Some(Error::InvalidState));
    }
    #[test]
    fn external_and_rejections() {
        let mut c = Conversation::new(Mechanism::external("").unwrap());
        c.begin().unwrap();
        assert_eq!(c.receive("AUTHENTICATE", &["+"]).lines, ["AUTHENTICATE +"]);
        assert_eq!(
            c.receive("AUTHENTICATE", &["+"]).error,
            Some(Error::UnexpectedChallenge)
        );
        for params in [
            vec![],
            vec![""],
            vec!["eA=="],
            vec!["+", "extra"],
            vec!["*"],
        ] {
            let mut c = Conversation::new(Mechanism::external("identity").unwrap());
            c.begin().unwrap();
            let step = c.receive("AUTHENTICATE", &params);
            assert_eq!(step.lines, ["AUTHENTICATE *"]);
            assert_eq!(step.error, Some(Error::UnexpectedChallenge));
        }
        assert!(Mechanism::plain("", "", "secret").is_err());
        assert!(Mechanism::plain("", "a\0b", "secret").is_err());
        assert!(Mechanism::plain("", "account", "").is_err());
        assert!(Mechanism::plain("", "account", "s\0ecret").is_err());
        assert!(Mechanism::plain("\0", "account", "secret").is_err());
        assert!(Mechanism::plain("", "a", &"p".repeat(MAX_RESPONSE_BYTES - 3)).is_ok());
        assert!(Mechanism::plain("", "a", &"p".repeat(MAX_RESPONSE_BYTES - 2)).is_err());
        assert!(Mechanism::external("\0").is_err());
        assert!(Mechanism::external(&"a".repeat(MAX_RESPONSE_BYTES + 1)).is_err());
        let external = Mechanism::external("identity").unwrap();
        assert_eq!(external.lines().unwrap(), encode(b"identity").unwrap());
        let mut premature = Conversation::new(external);
        assert_eq!(
            premature.receive("903", &[]).error,
            Some(Error::InvalidState)
        );
        premature.begin().unwrap();
        assert_eq!(
            premature.receive("903", &[]).error,
            Some(Error::InvalidState)
        );
        for numeric in ["901", "902", "904", "905", "906", "907"] {
            let mut c = Conversation::new(Mechanism::external("").unwrap());
            c.begin().unwrap();
            assert_eq!(c.receive(numeric, &["secret"]).error, Some(Error::Rejected));
        }
    }
}
