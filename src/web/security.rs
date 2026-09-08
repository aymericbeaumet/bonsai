use axum::http::{HeaderMap, Method, StatusCode, Uri, header};

#[derive(Clone)]
pub(super) struct Security {
    authority: String,
    origin: String,
    token: String,
}

impl Security {
    pub(super) fn new(port: u16, token: String) -> Self {
        let authority = format!("127.0.0.1:{port}");
        Self {
            origin: format!("http://{authority}"),
            authority,
            token,
        }
    }

    pub(super) fn launch_url(&self) -> String {
        format!("{}/#token={}", self.origin, self.token)
    }

    pub(super) fn content_security_policy(&self) -> String {
        format!(
            "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
             connect-src 'self' ws://{}; font-src 'self' data:; img-src 'self' data:; \
             object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'",
            self.authority
        )
    }

    pub(super) fn authorize(
        &self,
        method: &Method,
        uri: &Uri,
        headers: &HeaderMap,
    ) -> Result<(), (StatusCode, &'static str)> {
        if single_header(headers, header::HOST) != Some(self.authority.as_str()) {
            return Err((StatusCode::FORBIDDEN, "unrecognized server address"));
        }

        let websocket = uri.path().starts_with("/api/terminals/") && uri.path().ends_with("/ws");
        let mutating = !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS);
        let origin = single_header(headers, header::ORIGIN);
        if (websocket || mutating || headers.contains_key(header::ORIGIN))
            && origin != Some(self.origin.as_str())
        {
            return Err((
                StatusCode::FORBIDDEN,
                "request must come from this Bonsai page",
            ));
        }

        if uri.path() == "/api" || uri.path().starts_with("/api/") {
            let supplied = if websocket {
                websocket_token(uri)
            } else {
                single_header(headers, header::AUTHORIZATION)
                    .and_then(|value| value.strip_prefix("Bearer "))
            };
            if !supplied.is_some_and(|value| same_token(value, &self.token)) {
                return Err((
                    StatusCode::UNAUTHORIZED,
                    "open the link printed by bonsai hq",
                ));
            }
        }
        Ok(())
    }
}

fn single_header(headers: &HeaderMap, name: header::HeaderName) -> Option<&str> {
    let mut values = headers.get_all(name).iter();
    let first = values.next()?.to_str().ok()?;
    if values.next().is_some() {
        return None;
    }
    Some(first)
}

fn websocket_token(uri: &Uri) -> Option<&str> {
    let mut values = uri
        .query()?
        .split('&')
        .filter_map(|part| part.strip_prefix("token="));
    let first = values.next()?;
    if values.next().is_some() {
        return None;
    }
    Some(first)
}

fn same_token(left: &str, right: &str) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.bytes()
        .zip(right.bytes())
        .fold(0u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers() -> HeaderMap {
        HeaderMap::from_iter([
            (header::HOST, "127.0.0.1:4837".parse().unwrap()),
            (header::ORIGIN, "http://127.0.0.1:4837".parse().unwrap()),
            (header::AUTHORIZATION, "Bearer secret".parse().unwrap()),
        ])
    }

    fn authorize(method: Method, path: &str, headers: &HeaderMap) -> Result<(), StatusCode> {
        Security::new(4837, "secret".into())
            .authorize(&method, &path.parse().unwrap(), headers)
            .map_err(|(status, _)| status)
    }

    #[test]
    fn bearer_authentication_is_required_for_every_api_path() {
        for path in ["/api", "/api/state", "/api/terminals", "/api/missing"] {
            let mut request = headers();
            assert_eq!(authorize(Method::GET, path, &request), Ok(()));
            request.remove(header::AUTHORIZATION);
            assert_eq!(
                authorize(Method::GET, path, &request),
                Err(StatusCode::UNAUTHORIZED)
            );
            request.insert(header::AUTHORIZATION, "Bearer secreu".parse().unwrap());
            assert_eq!(
                authorize(Method::GET, path, &request),
                Err(StatusCode::UNAUTHORIZED)
            );
        }
    }

    #[test]
    fn websocket_requires_both_same_origin_and_query_token() {
        let mut request = headers();
        assert_eq!(
            authorize(Method::GET, "/api/terminals/1/ws", &request),
            Err(StatusCode::UNAUTHORIZED)
        );
        assert_eq!(
            authorize(Method::GET, "/api/terminals/1/ws?token=secret", &request),
            Ok(())
        );
        assert_eq!(
            authorize(
                Method::GET,
                "/api/terminals/1/ws?token=secret&token=secret",
                &request
            ),
            Err(StatusCode::UNAUTHORIZED)
        );
        request.remove(header::ORIGIN);
        assert_eq!(
            authorize(Method::GET, "/api/terminals/1/ws?token=secret", &request),
            Err(StatusCode::FORBIDDEN)
        );
    }

    #[test]
    fn refuses_dns_rebinding_and_cross_origin_even_with_valid_token() {
        for host in ["attacker.example:4837", "127.0.0.1:80", "localhost:4837"] {
            let mut request = headers();
            request.insert(header::HOST, host.parse().unwrap());
            assert_eq!(
                authorize(Method::GET, "/", &request),
                Err(StatusCode::FORBIDDEN)
            );
        }
        for origin in ["https://attacker.example", "null", "http://127.0.0.1:4838"] {
            let mut request = headers();
            request.insert(header::ORIGIN, origin.parse().unwrap());
            assert_eq!(
                authorize(Method::GET, "/api/state", &request),
                Err(StatusCode::FORBIDDEN)
            );
            assert_eq!(
                authorize(Method::POST, "/api/terminals", &request),
                Err(StatusCode::FORBIDDEN)
            );
        }
    }

    #[test]
    fn origin_optional_for_reads_but_required_for_mutations() {
        let mut request = headers();
        request.remove(header::ORIGIN);
        assert_eq!(authorize(Method::GET, "/api/state", &request), Ok(()));
        assert_eq!(authorize(Method::GET, "/", &request), Ok(()));
        for method in [Method::POST, Method::DELETE, Method::PUT] {
            assert_eq!(
                authorize(method, "/api/terminals", &request),
                Err(StatusCode::FORBIDDEN)
            );
        }
    }

    #[test]
    fn duplicate_security_headers_are_rejected() {
        let mut request = headers();
        request.append(header::HOST, "attacker.example".parse().unwrap());
        assert_eq!(
            authorize(Method::GET, "/", &request),
            Err(StatusCode::FORBIDDEN)
        );
        let mut request = headers();
        request.append(header::ORIGIN, "https://attacker.example".parse().unwrap());
        assert_eq!(
            authorize(Method::POST, "/api/terminals", &request),
            Err(StatusCode::FORBIDDEN)
        );
        let mut request = headers();
        request.append(header::AUTHORIZATION, "Bearer secret".parse().unwrap());
        assert_eq!(
            authorize(Method::GET, "/api/state", &request),
            Err(StatusCode::UNAUTHORIZED)
        );
    }
}
