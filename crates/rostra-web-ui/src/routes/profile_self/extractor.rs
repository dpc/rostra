use axum::body::Bytes;
use axum::extract::{FromRequest, Multipart, Request};
use axum::http::StatusCode;
use axum::http::header::CONTENT_TYPE;

pub struct InputForm {
    pub name: String,
    pub bio: String,
    pub avatar: Option<(String, Vec<u8>)>,
}
struct InputFormPart {
    pub name: Option<String>,
    pub bio: Option<String>,
    pub avatar: Option<(String, Vec<u8>)>,
}

impl<S> FromRequest<S> for InputForm
where
    Bytes: FromRequest<S>,
    S: Send + Sync,
{
    type Rejection = (StatusCode, &'static str);

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let Some(content_type) = req.headers().get(CONTENT_TYPE) else {
            return Err((StatusCode::BAD_REQUEST, "Missing content type"));
        };

        // Check if content type starts with "multipart/form-data" (it may include boundary parameter)
        if !content_type
            .to_str()
            .map(|s| s.starts_with("multipart/form-data"))
            .unwrap_or(false)
        {
            return Err((StatusCode::BAD_REQUEST, "Invalid content type"));
        }

        let mut multipart = Multipart::from_request(req, state)
            .await
            .map_err(|_| (StatusCode::BAD_REQUEST, "Failed to parse multipart"))?;

        let mut parts = InputFormPart {
            name: None,
            bio: None,
            avatar: None,
        };

        loop {
            let Some(field) = multipart
                .next_field()
                .await
                .map_err(|_| (StatusCode::BAD_REQUEST, "Failed to parse multipart field"))?
            else {
                break;
            };

            match field.name() {
                Some("bio") => {
                    let v = field.bytes().await.map_err(|_| {
                        (StatusCode::BAD_REQUEST, "Failed to parse multipart field")
                    })?;
                    let s = String::from_utf8(v.to_vec())
                        .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid encoding"))?;
                    if parts.bio.replace(s).is_some() {
                        return Err((StatusCode::BAD_REQUEST, "Failed to parse multipart field"));
                    }
                }
                Some("name") => {
                    let v = field.bytes().await.map_err(|_| {
                        (StatusCode::BAD_REQUEST, "Failed to parse multipart field")
                    })?;
                    let s = String::from_utf8(v.to_vec())
                        .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid encoding"))?;
                    if parts.name.replace(s).is_some() {
                        return Err((StatusCode::BAD_REQUEST, "Failed to parse multipart field"));
                    }
                }
                Some("avatar") => {
                    let Some(mime) = field.content_type().map(ToOwned::to_owned) else {
                        return Err((StatusCode::BAD_REQUEST, "Missing avatar mime type"));
                    };
                    let v = field.bytes().await.map_err(|_| {
                        (StatusCode::BAD_REQUEST, "Failed to parse multipart field")
                    })?;

                    if parts.avatar.replace((mime, v.to_vec())).is_some() {
                        return Err((StatusCode::BAD_REQUEST, "Failed to parse multipart field"));
                    }
                }
                _ => {
                    return Err((StatusCode::BAD_REQUEST, "Unknown field name"));
                }
            }
        }

        Ok(InputForm {
            name: parts
                .name
                .ok_or((StatusCode::BAD_REQUEST, "Failed to parse multipart field"))?,
            bio: parts
                .bio
                .ok_or((StatusCode::BAD_REQUEST, "Failed to parse multipart field"))?,
            avatar: parts.avatar,
        })
    }
}
