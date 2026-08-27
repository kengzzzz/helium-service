use axum::{
    body::Body,
    http::{HeaderValue, StatusCode, header::CONTENT_TYPE},
    response::{IntoResponse, Response},
};

#[derive(Debug, Clone)]
pub(crate) enum ServiceError {
    Status { status: StatusCode, text: String },
    Upstream { status: StatusCode },
    Internal,
}

impl ServiceError {
    pub(crate) fn with_status(status: StatusCode, text: impl Into<String>) -> Self {
        Self::Status {
            status,
            text: text.into(),
        }
    }

    pub(crate) fn bad_request(text: impl Into<String>) -> Self {
        Self::with_status(StatusCode::BAD_REQUEST, text)
    }

    pub(crate) fn internal(_: impl std::fmt::Display) -> Self {
        Self::Internal
    }

    pub(crate) fn upstream(category: &'static str, error: &reqwest::Error) -> Self {
        let (status, kind) = if error.is_timeout() {
            (StatusCode::GATEWAY_TIMEOUT, "timeout")
        } else if error.is_connect() {
            (StatusCode::BAD_GATEWAY, "connect")
        } else {
            (StatusCode::BAD_GATEWAY, "network")
        };
        crate::observability::upstream_failure(category, kind);
        Self::Upstream { status }
    }

    pub(crate) fn bad_gateway(category: &'static str, kind: &'static str) -> Self {
        crate::observability::upstream_failure(category, kind);
        Self::Upstream {
            status: StatusCode::BAD_GATEWAY,
        }
    }

    #[cfg(test)]
    pub(crate) fn status(&self) -> StatusCode {
        match self {
            Self::Status { status, .. } | Self::Upstream { status } => *status,
            Self::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for ServiceError {
    fn into_response(self) -> Response {
        match self {
            ServiceError::Status { status, text } => {
                let mut response = Response::new(Body::from(text));
                *response.status_mut() = status;
                response
                    .headers_mut()
                    .insert(CONTENT_TYPE, HeaderValue::from_static("text/plain"));
                response
            }
            ServiceError::Upstream { status } => {
                let mut response = Response::new(Body::from("upstream service unavailable"));
                *response.status_mut() = status;
                response
                    .headers_mut()
                    .insert(CONTENT_TYPE, HeaderValue::from_static("text/plain"));
                response
            }
            ServiceError::Internal => {
                let mut response = Response::new(Body::from("server error"));
                *response.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
                response
            }
        }
    }
}
