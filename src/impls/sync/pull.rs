use super::*;

pub async fn handler(
    svc: &mut SyncServer,
    request: Request<PullRequest>,
) -> ServerResult<Response<PullResponse>> {
    let host = svc.auth(&request).await?;
    let queue = svc.get_event_queue(&host)?;
    Ok(queue.into_response())
}