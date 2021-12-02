use tokio::sync::oneshot;

use super::*;

pub async fn handler(
    svc: &ChatServer,
    request: Request<()>,
    socket: Socket<StreamEventsResponse, StreamEventsRequest>,
) -> Result<(), HrpcServerError> {
    let user_id = svc.deps.valid_sessions.auth(&request)?;
    tracing::debug!("stream events validated for user {}", user_id);

    tracing::debug!("creating stream events for user {}", user_id);
    let (sub_tx, sub_rx) = mpsc::channel(64);
    let chat_tree = svc.deps.chat_tree.clone();

    let (tx, mut rx) = socket.split();

    let (close_by_send_tx, mut close_by_send_rx) = oneshot::channel();
    let (close_by_recv_tx, close_by_recv_rx) = oneshot::channel();

    let send_loop = svc.spawn_event_stream_processor(user_id, sub_rx, tx, close_by_recv_rx);
    let recv_loop = tokio::spawn(async move {
        loop {
            tokio::select! {
                res = rx.receive_message() => {
                    let req = bail_result!(res);
                    if let Some(req) = req.request {
                        use stream_events_request::*;

                        tracing::debug!("got new stream events request for user {}", user_id);

                        let sub = match req {
                            Request::SubscribeToGuild(SubscribeToGuild { guild_id }) => {
                                match chat_tree.check_guild_user(guild_id, user_id) {
                                    Ok(_) => EventSub::Guild(guild_id),
                                    Err(err) => {
                                        tracing::error!("{}", err);
                                        continue;
                                    }
                                }
                            }
                            Request::SubscribeToActions(SubscribeToActions {}) => EventSub::Actions,
                            Request::SubscribeToHomeserverEvents(SubscribeToHomeserverEvents {}) => {
                                EventSub::Homeserver
                            }
                        };

                        drop(sub_tx.send(sub).await);
                    }
                }
                _ = &mut close_by_send_rx => {
                    break;
                }
            }
        }
        #[allow(unreachable_code)]
        ServerResult::Ok(())
    });

    tokio::select!(
        res = send_loop => {
            drop(close_by_send_tx.send(()));
            if let Err(err) = res {
                panic!("stream events send loop task panicked: {}, aborting", err);
            }
        }
        res = recv_loop => {
            drop(close_by_recv_tx.send(()));
            match res {
                Ok(res) => res?,
                Err(err) => panic!("stream events recv loop task panicked: {}, aborting", err),
            }
        }
    );
    tracing::debug!("stream events ended for user {}", user_id);

    Ok(())
}
