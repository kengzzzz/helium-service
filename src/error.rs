use axum::{
    body::Body,
    http::{HeaderValue, StatusCode, header::CONTENT_TYPE},
    response::{IntoResponse, Response},
};

#[derive(Debug, Clone)]
pub(crate) enum ServiceError {
    Status { status: StatusCode, text: String },
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
            ServiceError::Internal => {
                let mut response = Response::new(Body::from("server error"));
                *response.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
                response
            }
        }
    }
}
