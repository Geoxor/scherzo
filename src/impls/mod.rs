pub mod against;
pub mod auth;
pub mod batch;
pub mod chat;
pub mod emote;
pub mod mediaproxy;
pub mod profile;
pub mod rest;
pub mod sync;
#[cfg(feature = "voice")]
pub mod voice;

use hrpc::client::transport::http::hyper::{http_client, HttpClient};
use hyper::{http, Uri};
use prelude::*;

use std::{str::FromStr, time::UNIX_EPOCH};

use dashmap::DashMap;
use harmony_rust_sdk::api::{exports::prost::bytes::Bytes, HomeserverIdentifier};
use parking_lot::Mutex;
use rand::Rng;
use tokio::sync::{broadcast, mpsc};

use crate::{config::Config, key, SharedConfig, SharedConfigData};

use self::{
    auth::AuthTree, chat::ChatTree, emote::EmoteTree, profile::ProfileTree, rest::RestServiceLayer,
    sync::EventDispatch,
};

pub mod prelude {
    pub use std::{convert::TryInto, mem::size_of};

    pub use crate::{
        db::{self, rkyv_arch, rkyv_ser, Batch, Db, DbResult, Tree},
        utils::evec::EVec,
        ServerError,
    };

    pub use harmony_rust_sdk::api::exports::prost::Message;
    pub use hrpc::{
        bail,
        response::IntoResponse,
        server::{
            error::{HrpcError as HrpcServerError, ServerResult},
            prelude::*,
            socket::Socket,
        },
        Request, Response,
    };
    pub use rkyv::Deserialize;
    pub use scherzo_derive::*;
    pub use smol_str::SmolStr;
    pub use triomphe::Arc;

    pub use super::{
        auth::{AuthExt, SessionMap},
        Dependencies,
    };

    pub(crate) use super::{impl_unary_handlers, impl_ws_handlers};
}

pub type FedEventReceiver = mpsc::UnboundedReceiver<EventDispatch>;
pub type FedEventDispatcher = mpsc::UnboundedSender<EventDispatch>;

pub struct Dependencies {
    pub auth_tree: AuthTree,
    pub chat_tree: ChatTree,
    pub profile_tree: ProfileTree,
    pub emote_tree: EmoteTree,
    pub sync_tree: Tree,

    pub valid_sessions: SessionMap,
    pub chat_event_sender: chat::EventSender,
    pub fed_event_dispatcher: FedEventDispatcher,
    pub key_manager: Option<Arc<key::Manager>>,
    pub action_processor: ActionProcesser,
    pub http: HttpClient,

    pub config: Config,
    pub runtime_config: SharedConfig,
}

impl Dependencies {
    pub async fn new(db: &Db, config: Config) -> DbResult<(Arc<Self>, FedEventReceiver)> {
        let (fed_event_dispatcher, fed_event_receiver) = mpsc::unbounded_channel();

        let auth_tree = AuthTree::new(db).await?;

        let this = Self {
            auth_tree: auth_tree.clone(),
            chat_tree: ChatTree::new(db).await?,
            profile_tree: ProfileTree::new(db).await?,
            emote_tree: EmoteTree::new(db).await?,
            sync_tree: db.open_tree(b"sync").await?,

            valid_sessions: Arc::new(DashMap::default()),
            chat_event_sender: broadcast::channel(2048).0,
            fed_event_dispatcher,
            key_manager: config
                .federation
                .as_ref()
                .map(|fc| Arc::new(key::Manager::new(fc.key.clone()))),
            action_processor: ActionProcesser { auth_tree },
            http: http_client(&mut hyper::Client::builder()),

            config,
            runtime_config: Arc::new(Mutex::new(SharedConfigData::default())),
        };

        Ok((Arc::new(this), fed_event_receiver))
    }
}

pub fn setup_server(
    deps: Arc<Dependencies>,
    fed_event_receiver: tokio::sync::mpsc::UnboundedReceiver<EventDispatch>,
    log_level: tracing::Level,
) -> (impl MakeRoutes, RestServiceLayer) {
    use self::{
        auth::AuthServer, batch::BatchServer, chat::ChatServer, emote::EmoteServer,
        mediaproxy::MediaproxyServer, profile::ProfileServer, sync::SyncServer,
    };
    use harmony_rust_sdk::api::{
        auth::auth_service_server::AuthServiceServer,
        batch::batch_service_server::BatchServiceServer,
        chat::chat_service_server::ChatServiceServer,
        emote::emote_service_server::EmoteServiceServer,
        mediaproxy::media_proxy_service_server::MediaProxyServiceServer,
        profile::profile_service_server::ProfileServiceServer,
        sync::postbox_service_server::PostboxServiceServer,
    };
    use hrpc::combine_services;

    let profile_server = ProfileServer::new(deps.clone());
    let emote_server = EmoteServer::new(deps.clone());
    let auth_server = AuthServer::new(deps.clone());
    let chat_server = ChatServer::new(deps.clone());
    let mediaproxy_server = MediaproxyServer::new(deps.clone());
    let sync_server = SyncServer::new(deps.clone(), fed_event_receiver);
    #[cfg(feature = "voice")]
    let voice_server = self::voice::VoiceServer::new(deps.clone(), log_level);

    let profile = ProfileServiceServer::new(profile_server.clone());
    let emote = EmoteServiceServer::new(emote_server);
    let auth = AuthServiceServer::new(auth_server);
    let chat = ChatServiceServer::new(chat_server.clone());
    let mediaproxy = MediaProxyServiceServer::new(mediaproxy_server);
    let sync = PostboxServiceServer::new(sync_server);
    #[cfg(feature = "voice")]
    let voice =
        harmony_rust_sdk::api::voice::voice_service_server::VoiceServiceServer::new(voice_server);

    let batchable_services = {
        let profile = ProfileServiceServer::new(profile_server.batch());
        let chat = ChatServiceServer::new(chat_server.batch());
        combine_services!(profile, chat)
    };

    let rest = RestServiceLayer::new(deps.clone());

    let batch_server = BatchServer::new(deps, batchable_services);
    let batch = BatchServiceServer::new(batch_server);

    let server = combine_services!(
        profile,
        emote,
        auth,
        chat,
        mediaproxy,
        sync,
        #[cfg(feature = "voice")]
        voice,
        batch
    );

    (server, rest)
}

fn get_time_secs() -> u64 {
    UNIX_EPOCH
        .elapsed()
        .expect("time is before unix epoch")
        .as_secs()
}

fn gen_rand_inline_str() -> SmolStr {
    // Safety: arrays generated by gen_rand_arr are alphanumeric, so they are valid ASCII chars as well as UTF-8 chars [ref:alphanumeric_array_gen]
    let arr = gen_rand_arr::<_, 22>(&mut rand::thread_rng());
    let str = unsafe { std::str::from_utf8_unchecked(&arr) };
    // Safety: generated array is exactly 22 u8s long
    SmolStr::new_inline(str)
}

#[allow(dead_code)]
fn gen_rand_str<const LEN: usize>() -> SmolStr {
    let arr = gen_rand_arr::<_, LEN>(&mut rand::thread_rng());
    // Safety: arrays generated by gen_rand_arr are alphanumeric, so they are valid ASCII chars as well as UTF-8 chars [ref:alphanumeric_array_gen]
    let str = unsafe { std::str::from_utf8_unchecked(&arr) };
    SmolStr::new(str)
}

fn gen_rand_arr<RNG: Rng, const LEN: usize>(rng: &mut RNG) -> [u8; LEN] {
    let mut res = [0_u8; LEN];

    let random = rng
        .sample_iter(rand::distributions::Alphanumeric) // [tag:alphanumeric_array_gen]
        .take(LEN);

    random
        .zip(res.iter_mut())
        .for_each(|(new_ch, ch)| *ch = new_ch);

    res
}

fn gen_rand_u64() -> u64 {
    rand::thread_rng().gen_range(1..u64::MAX)
}

fn get_mimetype<T>(response: &http::Response<T>) -> &str {
    response
        .headers()
        .get(&http::header::CONTENT_TYPE)
        .and_then(|val| val.to_str().ok())
        .and_then(|s| s.split(';').next())
        .unwrap_or("application/octet-stream")
}

fn get_content_length<T>(response: &http::Response<T>) -> http::HeaderValue {
    response
        .headers()
        .get(&http::header::CONTENT_LENGTH)
        .cloned()
        .unwrap_or_else(|| unsafe {
            http::HeaderValue::from_maybe_shared_unchecked(Bytes::from_static(b"0"))
        })
}

pub struct AdminActionError;

#[derive(Debug, Clone, Copy)]
pub enum AdminAction {
    GenerateRegistrationToken,
    Help,
}

impl FromStr for AdminAction {
    type Err = AdminActionError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let act = match s.trim_start_matches('/').trim() {
            "generate registration-token" => AdminAction::GenerateRegistrationToken,
            "help" => AdminAction::Help,
            _ => return Err(AdminActionError),
        };
        Ok(act)
    }
}

pub const HELP_TEXT: &str = r#"
commands are:
`generate registration-token` -> generates a registration token
`help` -> shows help
"#;

#[derive(Clone)]
pub struct ActionProcesser {
    auth_tree: AuthTree,
}

impl ActionProcesser {
    pub async fn run(&self, action: &str) -> ServerResult<String> {
        let maybe_action = AdminAction::from_str(action);
        match maybe_action {
            Ok(action) => match action {
                AdminAction::GenerateRegistrationToken => {
                    let token = self.auth_tree.put_rand_reg_token().await?;
                    Ok(token.into())
                }
                AdminAction::Help => Ok(HELP_TEXT.to_string()),
            },
            Err(_) => Ok(format!("invalid command: `{}`", action)),
        }
    }
}

macro_rules! impl_unary_handlers {
    ($(
        $( #[$attr:meta] )*
        $handler:ident, $req:ty, $resp:ty;
    )+) => {
        $(
            $( #[$attr] )*
            fn $handler(&self, request: Request<$req>) -> hrpc::exports::futures_util::future::BoxFuture<'_, ServerResult<Response<$resp>>> {
                Box::pin($handler::handler(self, request))
            }
        )+
    };
}

macro_rules! impl_ws_handlers {
    ($(
        $( #[$attr:meta] )*
        $handler:ident, $req:ty, $resp:ty;
    )+) => {
        $(
            $( #[$attr] )*
            fn $handler(&self, request: Request<()>, socket: Socket<$resp, $req>) -> hrpc::exports::futures_util::future::BoxFuture<'_, ServerResult<()>> {
                Box::pin($handler::handler(self, request, socket))
            }
        )+
    };
}

pub(crate) use impl_unary_handlers;
pub(crate) use impl_ws_handlers;
