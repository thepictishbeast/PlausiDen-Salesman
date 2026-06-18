//! Basic-auth middleware. Constant-time compare on the credential.

use axum::http::{HeaderValue, Request, StatusCode, header};
use axum::middleware::Next;
use axum::response::Response;

/// Axum middleware enforcing HTTP Basic auth against `expected`
/// ("user:pass"), using a constant-time credential compare. `/healthz`
/// bypasses auth; a missing or incorrect credential yields 401 with a
/// `WWW-Authenticate` challenge.
pub async fn basic_auth(expected: String, req: Request<axum::body::Body>, next: Next) -> Response {
    // /healthz bypass — load balancers shouldn't need creds for liveness.
    if req.uri().path() == "/healthz" {
        return next.run(req).await;
    }

    let provided = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Basic "))
        .and_then(|b64| {
            // Decode base64 → "user:pass" string.
            use base64::Engine;
            base64::engine::general_purpose::STANDARD
                .decode(b64.as_bytes())
                .ok()
                .and_then(|bytes| String::from_utf8(bytes).ok())
        });

    let ok = match provided {
        Some(creds) => constant_time_eq(creds.as_bytes(), expected.as_bytes()),
        None => false,
    };

    if ok {
        next.run(req).await
    } else {
        let mut resp = Response::new(axum::body::Body::from("auth required\n"));
        *resp.status_mut() = StatusCode::UNAUTHORIZED;
        resp.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            HeaderValue::from_static("Basic realm=\"salesman\""),
        );
        resp
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    use subtle::ConstantTimeEq;
    a.ct_eq(b).into()
}
