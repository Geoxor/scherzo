use super::*;

pub async fn handler(
    svc: &mut EmoteServer,
    request: Request<DeleteEmoteFromPackRequest>,
) -> ServerResult<Response<DeleteEmoteFromPackResponse>> {
    #[allow(unused_variables)]
    let user_id = svc.valid_sessions.auth(&request)?;

    let DeleteEmoteFromPackRequest { pack_id, name } = request.into_message().await?;

    svc.emote_tree.check_if_emote_pack_owner(pack_id, user_id)?;

    let key = make_emote_pack_emote_key(pack_id, &name);

    svc.emote_tree.remove(key)?;

    let equipped_users = svc.emote_tree.calculate_users_pack_equipped(pack_id)?;
    svc.send_event_through_chan(
        EventSub::Homeserver,
        stream_event::Event::EmotePackEmotesUpdated(EmotePackEmotesUpdated {
            pack_id,
            added_emotes: Vec::new(),
            deleted_emotes: vec![name],
        }),
        None,
        EventContext::new(equipped_users),
    );

    Ok((DeleteEmoteFromPackResponse {}).into_response())
}