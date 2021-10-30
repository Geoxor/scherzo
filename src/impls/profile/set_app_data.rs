use super::*;

pub async fn handler(
    svc: &mut ProfileServer,
    request: Request<SetAppDataRequest>,
) -> ServerResult<Response<SetAppDataResponse>> {
    #[allow(unused_variables)]
    let user_id = svc.valid_sessions.auth(&request)?;

    let SetAppDataRequest { app_id, app_data } = request.into_message().await?;
    svc.profile_tree
        .insert(make_user_metadata_key(user_id, &app_id), app_data)?;

    Ok((SetAppDataResponse {}).into_response())
}