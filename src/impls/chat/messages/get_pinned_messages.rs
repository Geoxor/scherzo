use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<GetPinnedMessagesRequest>,
) -> ServerResult<Response<GetPinnedMessagesResponse>> {
    let user_id = svc.deps.valid_sessions.auth(&request)?;

    let GetPinnedMessagesRequest {
        guild_id,
        channel_id,
    } = request.into_message().await?;

    let chat_tree = &svc.deps.chat_tree;

    chat_tree
        .check_guild_user_channel(guild_id, user_id, channel_id)
        .await?;

    chat_tree
        .check_perms(
            guild_id,
            Some(channel_id),
            user_id,
            all_permissions::MESSAGES_VIEW,
            false,
        )
        .await?;

    let pinned_message_ids = chat_tree
        .get_pinned_messages_logic(guild_id, channel_id)
        .await?;

    Ok((GetPinnedMessagesResponse { pinned_message_ids }).into_response())
}
