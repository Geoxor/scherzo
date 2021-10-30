use super::{
    chat::{ChatTree, EventBroadcast, EventContext, EventSender, EventSub, PermCheck},
    prelude::*,
};

use db::profile::*;
use harmony_rust_sdk::api::{
    chat::Event,
    profile::{profile_service_server::ProfileService, *},
};

pub mod get_app_data;
pub mod get_profile;
pub mod set_app_data;
pub mod update_profile;

#[derive(Clone)]
pub struct ProfileServer {
    profile_tree: ProfileTree,
    chat_tree: ChatTree,
    valid_sessions: SessionMap,
    pub broadcast_send: EventSender,
    disable_ratelimits: bool,
}

impl ProfileServer {
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            profile_tree: deps.profile_tree.clone(),
            chat_tree: deps.chat_tree.clone(),
            valid_sessions: deps.valid_sessions.clone(),
            broadcast_send: deps.chat_event_sender.clone(),
            disable_ratelimits: deps.config.policy.disable_ratelimits,
        }
    }

    #[inline(always)]
    fn send_event_through_chan(
        &self,
        sub: EventSub,
        event: stream_event::Event,
        perm_check: Option<PermCheck<'static>>,
        context: EventContext,
    ) {
        let broadcast = EventBroadcast::new(sub, Event::Profile(event), perm_check, context);

        drop(self.broadcast_send.send(Arc::new(broadcast)));
    }
}

impl ProfileService for ProfileServer {
    impl_unary_handlers! {
        #[rate(5, 10)]
        get_profile, GetProfileRequest, GetProfileResponse;
        #[rate(4, 1)]
        get_app_data, GetAppDataRequest, GetAppDataResponse;
        #[rate(2, 5)]
        set_app_data, SetAppDataRequest, SetAppDataResponse;
        #[rate(4, 5)]
        update_profile, UpdateProfileRequest, UpdateProfileResponse;
    }
}

#[derive(Clone)]
pub struct ProfileTree {
    pub inner: ArcTree,
}

impl ProfileTree {
    impl_db_methods!(inner);

    pub fn new(db: &dyn Db) -> DbResult<Self> {
        let inner = db.open_tree(b"profile")?;
        Ok(Self { inner })
    }

    pub fn update_profile_logic(
        &self,
        user_id: u64,
        new_user_name: Option<String>,
        new_user_avatar: Option<String>,
        new_user_status: Option<i32>,
        new_is_bot: Option<bool>,
    ) -> ServerResult<()> {
        let key = make_user_profile_key(user_id);

        let mut profile = self
            .get(key)?
            .map_or_else(Profile::default, db::deser_profile);

        if let Some(new_username) = new_user_name {
            profile.user_name = new_username;
        }
        if let Some(new_avatar) = new_user_avatar {
            profile.user_avatar = Some(new_avatar);
        }
        if let Some(new_status) = new_user_status {
            profile.user_status = new_status;
        }
        if let Some(new_is_bot) = new_is_bot {
            profile.is_bot = new_is_bot;
        }

        let buf = rkyv_ser(&profile);
        self.insert(key, buf)?;

        Ok(())
    }

    pub fn get_profile_logic(&self, user_id: u64) -> ServerResult<Profile> {
        let key = make_user_profile_key(user_id);

        let profile = if let Some(profile_raw) = self.get(key)? {
            db::deser_profile(profile_raw)
        } else {
            return Err(ServerError::NoSuchUser(user_id).into());
        };

        Ok(profile)
    }

    pub fn does_user_exist(&self, user_id: u64) -> ServerResult<()> {
        self.contains_key(&make_user_profile_key(user_id))?
            .then(|| Ok(()))
            .unwrap_or_else(|| Err(ServerError::NoSuchUser(user_id).into()))
    }

    /// Converts a local user ID to the corresponding foreign user ID and the host
    pub fn local_to_foreign_id(&self, local_id: u64) -> ServerResult<Option<(u64, SmolStr)>> {
        let key = make_local_to_foreign_user_key(local_id);

        Ok(self.get(key)?.map(|raw| {
            let (raw_id, raw_host) = raw.split_at(size_of::<u64>());
            // Safety: safe since we split at u64 boundary.
            let foreign_id = u64::from_be_bytes(unsafe { raw_id.try_into().unwrap_unchecked() });
            // Safety: all stored hosts are valid UTF-8
            let host = (unsafe { std::str::from_utf8_unchecked(raw_host) }).into();
            (foreign_id, host)
        }))
    }

    /// Convert a foreign user ID to a local user ID
    pub fn foreign_to_local_id(&self, foreign_id: u64, host: &str) -> ServerResult<Option<u64>> {
        let key = make_foreign_to_local_user_key(foreign_id, host);

        Ok(self
            .get(key)?
            // Safety: we store u64's only for these keys
            .map(|raw| u64::from_be_bytes(unsafe { raw.try_into().unwrap_unchecked() })))
    }
}