use std::sync::Arc;

use axum::extract::{Query, Request, State};
use axum::http::{HeaderValue, header};
use axum::middleware::Next;
use axum::response::Response;
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode};
use serde::Deserialize;
use tracing::warn;

use crate::config::AuthConfig;
use crate::error::AppError;

#[derive(Clone)]
pub struct AuthState {
    auth: Option<AuthConfig>,
}

impl AuthState {
    pub fn new(auth: Option<AuthConfig>) -> Self {
        Self { auth }
    }

    pub fn is_enabled(&self) -> bool {
        self.auth.is_some()
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct AuthQuery {
    pub access_token: Option<String>,
    pub token: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct JwtClaims {}

pub async fn auth_middleware(
    State(state): State<Arc<AuthState>>,
    Query(query): Query<AuthQuery>,
    req: Request,
    next: Next,
) -> Result<Response, AppError> {
    if !state.is_enabled() {
        return Ok(next.run(req).await);
    }

    let path = req.uri().path().to_string();

    let token = bearer_token(req.headers().get(header::AUTHORIZATION))
        .or(query.access_token)
        .or(query.token)
        .ok_or_else(|| {
            warn!(path = %path, "missing bearer token");
            AppError::unauthorized("Missing bearer token")
        })?;

    if let Err(error) = validate_jwt(state.auth.as_ref().expect("auth enabled"), &token) {
        warn!(path = %path, error = %error, "invalid bearer token");
        return Err(error);
    }
    Ok(next.run(req).await)
}

fn bearer_token(header_value: Option<&HeaderValue>) -> Option<String> {
    let value = header_value?.to_str().ok()?;
    let token = value.strip_prefix("Bearer ")?;
    let trimmed = token.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn validate_jwt(auth: &AuthConfig, token: &str) -> Result<(), AppError> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.validate_exp = true;
    validation.required_spec_claims.insert("exp".to_string());

    decode::<JwtClaims>(
        token,
        &DecodingKey::from_secret(auth.jwt_secret.as_bytes()),
        &validation,
    )
    .map(|_| ())
    .map_err(|error| AppError::unauthorized(format!("Invalid bearer token: {error}")))
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;
    use jsonwebtoken::{EncodingKey, Header, encode};
    use serde_json::json;

    use super::*;

    #[test]
    fn bearer_token_extracts_trimmed_token() {
        let header = HeaderValue::from_static("Bearer token-value ");

        assert_eq!(bearer_token(Some(&header)), Some("token-value".to_string()));
    }

    #[test]
    fn bearer_token_rejects_missing_or_empty_bearer() {
        let wrong_scheme = HeaderValue::from_static("Basic token-value");
        let empty = HeaderValue::from_static("Bearer   ");

        assert_eq!(bearer_token(Some(&wrong_scheme)), None);
        assert_eq!(bearer_token(Some(&empty)), None);
        assert_eq!(bearer_token(None), None);
    }

    #[test]
    fn validate_jwt_accepts_hs256_token_with_exp() {
        let auth = AuthConfig {
            jwt_secret: "secret".to_string(),
        };
        let claims = json!({
            "exp": 4_102_444_800_i64
        });
        let token = encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(auth.jwt_secret.as_bytes()),
        )
        .unwrap();

        assert!(validate_jwt(&auth, &token).is_ok());
    }

    #[test]
    fn validate_jwt_rejects_token_without_exp() {
        let auth = AuthConfig {
            jwt_secret: "secret".to_string(),
        };
        let claims = json!({});
        let token = encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(auth.jwt_secret.as_bytes()),
        )
        .unwrap();

        assert!(validate_jwt(&auth, &token).is_err());
    }
}
