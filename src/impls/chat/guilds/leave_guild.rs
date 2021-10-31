use super::*;

pub async fn handler(
    svc: &mut ChatServer,
    request: Request<LeaveGuildRequest>,
) -> ServerResult<Response<LeaveGuildResponse>> {
    let user_id = svc.deps.valid_sessions.auth(&request)?;

    let LeaveGuildRequest { guild_id } = request.into_message().await?;

    let chat_tree = &svc.deps.chat_tree;

    chat_tree.check_guild_user(guild_id, user_id)?;

    chat_tree
        .chat_tree
        .remove(&make_member_key(guild_id, user_id))
        .map_err(ServerError::DbError)?;

    svc.send_event_through_chan(
        EventSub::Guild(guild_id),
        stream_event::Event::LeftMember(stream_event::MemberLeft {
            guild_id,
            member_id: user_id,
            leave_reason: LeaveReason::WillinglyUnspecified.into(),
        }),
        None,
        EventContext::empty(),
    );

    svc.dispatch_guild_leave(guild_id, user_id)?;

    Ok((LeaveGuildResponse {}).into_response())
}