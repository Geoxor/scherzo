use super::*;

pub async fn handler(
    svc: &mut EmoteServer,
    request: Request<DequipEmotePackRequest>,
) -> ServerResult<Response<DequipEmotePackResponse>> {
    #[allow(unused_variables)]
    let user_id = svc.valid_sessions.auth(&request)?;

    let DequipEmotePackRequest { pack_id } = request.into_message().await?;

    svc.emote_tree.dequip_emote_pack_logic(user_id, pack_id)?;

    svc.send_event_through_chan(
        EventSub::Homeserver,
        stream_event::Event::EmotePackDeleted(EmotePackDeleted { pack_id }),
        None,
        EventContext::new(vec![user_id]),
    );

    Ok((DequipEmotePackResponse {}).into_response())
}