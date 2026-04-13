use std::sync::Arc;

use axum::extract::{Query, Request, State};
use axum::http::{HeaderValue, header};
use axum::middleware::Next;
use axum::response::Response;
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode};
use serde::Deserialize;

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

    let token = bearer_token(req.headers().get(header::AUTHORIZATION))
        .or(query.access_token)
        .or(query.token)
        .ok_or_else(|| AppError::unauthorized("Missing bearer token"))?;

    validate_jwt(state.auth.as_ref().expect("auth enabled"), &token)?;
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
