use super::*;

pub async fn handler(
    _svc: &mut ChatServer,
    _request: Request<PinMessageRequest>,
) -> ServerResult<Response<PinMessageResponse>> {
    Err(ServerError::NotImplemented.into())
}