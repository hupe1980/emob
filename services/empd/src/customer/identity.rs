//! Ephemeral customer sessions; grant data is always read from the owner's current book.
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use emob_service::Token;
use sha2::{Digest as _, Sha256};

const IDLE: Duration = Duration::from_mins(30);
const LIFETIME: Duration = Duration::from_hours(8);
const WINDOW: Duration = Duration::from_mins(1);
const LIMIT: u8 = 8;
const SESSIONS_PER_SUBJECT: usize = 4;

struct Session {
    subject: String,
    issued: Instant,
    seen: Instant,
}

impl Session {
    fn current(&self, now: Instant) -> bool {
        now.duration_since(self.issued) < LIFETIME && now.duration_since(self.seen) < IDLE
    }
}

type Failures = Option<(Instant, u8)>;

struct User {
    verifier: Token,
    failures: Failures,
}

pub(crate) struct Identity {
    users: BTreeMap<String, User>,
    sessions: BTreeMap<[u8; 32], Session>,
    unknown_failures: [Failures; 256],
    dummy: Token,
}

impl Identity {
    pub(crate) fn new(users: BTreeMap<String, Token>) -> Self {
        Self {
            users: users
                .into_iter()
                .map(|(name, verifier)| {
                    (
                        name,
                        User {
                            verifier,
                            failures: None,
                        },
                    )
                })
                .collect(),
            sessions: BTreeMap::new(),
            unknown_failures: [None; 256],
            dummy: Token::new(&"0".repeat(64)),
        }
    }

    pub(super) fn forget(&mut self, subject: &str) {
        self.users.remove(subject);
        self.sessions
            .retain(|_, session| session.subject != subject);
    }

    pub(super) fn contains(&self, subject: &str) -> bool {
        self.users.contains_key(subject)
    }

    pub(super) fn login(
        &mut self,
        subject: &str,
        secret: &str,
        now: Instant,
    ) -> Result<String, StatusCode> {
        let (verifier, failures, known) = if let Some(user) = self.users.get_mut(subject) {
            (&user.verifier, &mut user.failures, true)
        } else {
            let bucket = usize::from(Sha256::digest(subject.as_bytes())[0]);
            (&self.dummy, &mut self.unknown_failures[bucket], false)
        };
        if failures.is_some_and(|(start, _)| now.duration_since(start) >= WINDOW) {
            *failures = None;
        }
        if failures.is_some_and(|(_, count)| count >= LIMIT) {
            return Err(StatusCode::TOO_MANY_REQUESTS);
        }
        let matches = verifier.verify(&hex::encode(Sha256::digest(secret.as_bytes())));
        if !matches || !known {
            failures.get_or_insert((now, 0)).1 += 1;
            return Err(StatusCode::UNAUTHORIZED);
        }
        *failures = None;
        let mut bytes = [0_u8; 32];
        aws_lc_rs::rand::fill(&mut bytes).map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        let token = hex::encode(bytes);
        self.sessions.retain(|_, session| session.current(now));
        let yours: Vec<_> = self
            .sessions
            .iter()
            .filter(|(_, s)| s.subject == subject)
            .collect();
        if yours.len() >= SESSIONS_PER_SUBJECT
            && let Some((&key, _)) = yours.into_iter().min_by_key(|(_, s)| s.issued)
        {
            self.sessions.remove(&key);
        }
        self.sessions.insert(
            Sha256::digest(token.as_bytes()).into(),
            Session {
                subject: subject.into(),
                issued: now,
                seen: now,
            },
        );
        Ok(token)
    }

    pub(super) fn subject(&mut self, token: &str, now: Instant) -> Result<String, StatusCode> {
        let key: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        let session = self
            .sessions
            .get_mut(&key)
            .ok_or(StatusCode::UNAUTHORIZED)?;
        if !session.current(now) {
            self.sessions.remove(&key);
            return Err(StatusCode::UNAUTHORIZED);
        }
        session.seen = now;
        Ok(session.subject.clone())
    }

    pub(super) fn logout(&mut self, token: &str, now: Instant) -> Result<(), StatusCode> {
        let key: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        self.sessions
            .remove(&key)
            .filter(|s| s.current(now))
            .map(|_| ())
            .ok_or(StatusCode::UNAUTHORIZED)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // customer-invoice-access/SR-341
    #[test]
    fn sessions_expire_and_sign_in_attempts_are_bounded() {
        let key = "47".repeat(32);
        let users = BTreeMap::from([(
            "river".into(),
            Token::new(&hex::encode(Sha256::digest(key.as_bytes()))),
        )]);
        let mut id = Identity::new(users);
        let now = Instant::now();
        for _ in 0..LIMIT {
            assert_eq!(
                id.login("river", "wrong", now),
                Err(StatusCode::UNAUTHORIZED)
            );
        }
        assert_eq!(
            id.login("river", &key, now),
            Err(StatusCode::TOO_MANY_REQUESTS)
        );
        let start = now + WINDOW;
        let token = id.login("river", &key, start).unwrap();
        assert_eq!(id.subject(&key, start), Err(StatusCode::UNAUTHORIZED));
        assert_eq!(
            id.subject(&token, start + IDLE),
            Err(StatusCode::UNAUTHORIZED)
        );
        let token = id.login("river", &key, start + IDLE).unwrap();
        for n in 1..32 {
            assert_eq!(
                id.subject(&token, start + IDLE + n * IDLE / 2).unwrap(),
                "river"
            );
        }
        assert_eq!(
            id.subject(&token, start + IDLE + LIFETIME),
            Err(StatusCode::UNAUTHORIZED)
        );
    }
}
