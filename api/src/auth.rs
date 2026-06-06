use std::time::{SystemTime, UNIX_EPOCH};
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation, Algorithm};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub role: String,
    pub exp: usize,
    pub iat: usize,
}

pub struct JwtAuth {
    secret: String,
    expire_secs: u64,
}

impl JwtAuth {
    pub fn new(secret: String, expire_secs: u64) -> Self {
        Self { secret, expire_secs }
    }

    pub fn generate_token(&self, username: &str, role: &str) -> Result<String, jsonwebtoken::errors::Error> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as usize;

        let claims = Claims {
            sub: username.to_string(),
            role: role.to_string(),
            iat: now,
            exp: now + self.expire_secs as usize,
        };

        encode(
            &Header::default(),
            &claims,
            &EncodingKey::from_secret(self.secret.as_bytes()),
        )
    }

    pub fn validate_token(&self, token: &str) -> Result<Claims, jsonwebtoken::errors::Error> {
        let mut validation = Validation::new(Algorithm::HS256);
        validation.validate_exp = true;

        let token_data = decode::<Claims>(
            token,
            &DecodingKey::from_secret(self.secret.as_bytes()),
            &validation,
        )?;

        Ok(token_data.claims)
    }

    pub fn expire_secs(&self) -> u64 {
        self.expire_secs
    }
}
