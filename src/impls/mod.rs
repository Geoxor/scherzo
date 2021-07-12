pub mod auth;
pub mod chat;
pub mod mediaproxy;
pub mod rest;
pub mod sync;

use std::{
    convert::TryInto,
    mem::size_of,
    time::{Duration, UNIX_EPOCH},
};

use ed25519_compact::PublicKey;
use harmony_rust_sdk::api::{
    exports::{
        hrpc::{
            http,
            server::filters::{rate::Rate, rate_limit},
            warp::{self, filters::BoxedFilter, Filter, Reply},
        },
        prost::bytes::Bytes,
    },
    harmonytypes::Token,
};
use rand::Rng;
use reqwest::Response;
use smol_str::SmolStr;

use crate::ServerError;

fn get_time_secs() -> u64 {
    UNIX_EPOCH
        .elapsed()
        .expect("time is before unix epoch")
        .as_secs()
}

fn gen_rand_inline_str() -> SmolStr {
    // Safety: arrays generated by gen_rand_arr are alphanumeric, so they are valid ASCII chars as well as UTF-8 chars [tag:inlined_smol_str_gen] [ref:alphanumeric_array_gen]
    let arr = gen_rand_arr::<22>();
    let str = unsafe { std::str::from_utf8_unchecked(&arr) };
    // Safety: generated array is exactly 22 u8s long
    SmolStr::new_inline(str)
}

#[allow(dead_code)]
fn gen_rand_str<const LEN: usize>() -> SmolStr {
    let arr = gen_rand_arr::<LEN>();
    // Safety: arrays generated by gen_rand_arr are alphanumeric, so they are valid ASCII chars as well as UTF-8 chars [ref:alphanumeric_array_gen]
    let str = unsafe { std::str::from_utf8_unchecked(&arr) };
    SmolStr::new(str)
}

fn gen_rand_arr<const LEN: usize>() -> [u8; LEN] {
    let mut res = [0_u8; LEN];

    let random = rand::thread_rng()
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

fn rate(num: u64, dur: u64) -> BoxedFilter<()> {
    rate_limit(
        Rate::new(num, Duration::from_secs(dur)),
        ServerError::TooFast,
    )
    .boxed()
}

fn get_mimetype(response: &Response) -> &str {
    response
        .headers()
        .get(&http::header::CONTENT_TYPE)
        .and_then(|val| val.to_str().ok())
        .and_then(|s| s.split(';').next())
        .unwrap_or("application/octet-stream")
}

fn get_content_length(response: &Response) -> http::HeaderValue {
    response
        .headers()
        .get(&http::header::CONTENT_LENGTH)
        .cloned()
        .unwrap_or_else(|| unsafe {
            http::HeaderValue::from_maybe_shared_unchecked(Bytes::from_static(b"0"))
        })
}

#[inline(always)]
fn make_u64_iter_logic(raw: &[u8]) -> impl Iterator<Item = u64> + '_ {
    raw.chunks_exact(size_of::<u64>())
        .map(|raw| u64::from_be_bytes(raw.try_into().unwrap()))
}

const SCHERZO_VERSION: &str = git_version::git_version!(
    prefix = "git:",
    cargo_prefix = "cargo:",
    fallback = "unknown"
);

pub fn version() -> BoxedFilter<(impl Reply,)> {
    warp::get()
        .and(warp::path!("_harmony" / "version"))
        .map(|| format!("scherzo {}\n", SCHERZO_VERSION))
        .boxed()
}

const KEY_TAG: &str = "ED25519 PUBLIC KEY";

pub fn verify_token(token: &Token, pubkey: &PublicKey) -> Result<(), ServerError> {
    let Token { sig, data } = token;

    let sig = ed25519_compact::Signature::from_slice(sig.as_slice())
        .map_err(|_| ServerError::InvalidTokenSignature)?;

    pubkey
        .verify(data, &sig)
        .map_err(|_| ServerError::CouldntVerifyTokenData)
}

// Taken from `lockless` (license https://github.com/Diggsey/lockless/blob/master/Cargo.toml#L7)
// and modified
pub mod append_list {
    use std::ops::Not;
    use std::sync::atomic::{AtomicPtr, Ordering};
    use std::{mem, ptr};

    type NodePtr<T> = Option<Box<Node<T>>>;

    #[derive(Debug)]
    struct Node<T> {
        value: T,
        next: AppendList<T>,
    }

    #[derive(Debug)]
    pub struct AppendList<T>(AtomicPtr<Node<T>>);

    impl<T> AppendList<T> {
        #[allow(clippy::new_without_default)]
        pub fn new() -> Self {
            Self::new_internal(None)
        }

        pub fn append(&self, value: T) {
            self.append_list(AppendList::new_internal(Some(Box::new(Node {
                value,
                next: AppendList::new(),
            }))));
        }

        pub fn append_list(&self, other: AppendList<T>) {
            let p = other.0.load(Ordering::Acquire);
            mem::forget(other);
            unsafe { self.append_ptr(p) };
        }

        pub const fn iter(&self) -> AppendListIterator<T> {
            AppendListIterator(&self.0)
        }

        #[allow(clippy::wrong_self_convention)]
        fn into_raw(ptr: NodePtr<T>) -> *mut Node<T> {
            match ptr {
                Some(b) => Box::into_raw(b),
                None => ptr::null_mut(),
            }
        }

        unsafe fn from_raw(ptr: *mut Node<T>) -> NodePtr<T> {
            ptr.is_null().not().then(|| Box::from_raw(ptr))
        }

        fn new_internal(ptr: NodePtr<T>) -> Self {
            AppendList(AtomicPtr::new(Self::into_raw(ptr)))
        }

        unsafe fn append_ptr(&self, p: *mut Node<T>) {
            loop {
                match self.0.compare_exchange_weak(
                    ptr::null_mut(),
                    p,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => return,
                    Err(head) => {
                        if !head.is_null() {
                            return (*head).next.append_ptr(p);
                        }
                    }
                }
            }
        }
    }

    impl<T> Drop for AppendList<T> {
        fn drop(&mut self) {
            unsafe { Self::from_raw(mem::replace(self.0.get_mut(), ptr::null_mut())) };
        }
    }

    impl<'a, T> IntoIterator for &'a AppendList<T> {
        type Item = &'a T;
        type IntoIter = AppendListIterator<'a, T>;

        fn into_iter(self) -> AppendListIterator<'a, T> {
            self.iter()
        }
    }

    #[derive(Debug)]
    pub struct AppendListIterator<'a, T: 'a>(&'a AtomicPtr<Node<T>>);

    impl<'a, T: 'a> Iterator for AppendListIterator<'a, T> {
        type Item = &'a T;

        fn next(&mut self) -> Option<&'a T> {
            let p = self.0.load(Ordering::Acquire);
            p.is_null().not().then(|| unsafe {
                self.0 = &(*p).next.0;
                &(*p).value
            })
        }
    }
}

pub mod keys_manager {
    use std::path::PathBuf;

    use super::*;

    use ahash::RandomState;
    use dashmap::{mapref::one::RefMut, DashMap};
    use ed25519_compact::{KeyPair, PublicKey, Seed};
    use harmony_rust_sdk::api::{
        auth::auth_service_client::AuthServiceClient,
        sync::postbox_service_client::PostboxServiceClient,
    };

    use reqwest::Url;

    type Clients = (PostboxServiceClient, AuthServiceClient);

    fn parse_pem(key: String, host: &str) -> Result<ed25519_compact::PublicKey, ServerError> {
        let pem = pem::parse(key).map_err(|_| ServerError::CantGetHostKey(host.into()))?;

        if pem.tag != KEY_TAG {
            return Err(ServerError::CantGetHostKey(host.into()));
        }

        ed25519_compact::PublicKey::from_slice(pem.contents.as_slice())
            .map_err(|_| ServerError::CantGetHostKey(host.into()))
    }

    #[derive(Debug)]
    pub struct KeysManager {
        keys: DashMap<String, PublicKey, RandomState>,
        clients: DashMap<String, Clients, RandomState>,
        federation_key: PathBuf,
    }

    impl KeysManager {
        pub fn new(federation_key: PathBuf) -> Self {
            Self {
                federation_key,
                keys: DashMap::default(),
                clients: DashMap::default(),
            }
        }

        pub fn invalidate_key(&self, host: &str) {
            self.keys.remove(host);
        }

        pub async fn get_own_key(&self) -> Result<KeyPair, ServerError> {
            match tokio::fs::read(&self.federation_key).await {
                Ok(key) => {
                    ed25519_compact::KeyPair::from_slice(&key).map_err(|_| ServerError::CantGetKey)
                }
                Err(err) => {
                    if err.kind() == std::io::ErrorKind::NotFound {
                        let new_key = ed25519_compact::KeyPair::from_seed(Seed::generate());
                        tokio::fs::write(&self.federation_key, new_key.as_ref())
                            .await
                            .map(|_| new_key)
                            .map_err(|_| ServerError::CantGetKey)
                    } else {
                        Err(ServerError::CantGetKey)
                    }
                }
            }
        }

        pub async fn get_key(&self, host: &str) -> Result<PublicKey, ServerError> {
            let key = if let Some(key) = self.keys.get(host) {
                *key
            } else {
                let key = self
                    .get_client(host)
                    .1
                    .key(())
                    .await
                    .map_err(|_| ServerError::CantGetHostKey(host.into()))?
                    .key;
                let key = parse_pem(key, host)?;
                self.keys.insert(host.to_string(), key);
                key
            };

            Ok(key)
        }

        fn get_client<'a>(&'a self, host: &str) -> RefMut<'a, String, Clients, RandomState> {
            if let Some(client) = self.clients.get_mut(host) {
                client
            } else {
                let http = reqwest::Client::new(); // each server gets its own http client
                let host_url: Url = host.parse().unwrap();

                let sync_client =
                    PostboxServiceClient::new(http.clone(), host_url.clone()).unwrap();
                let auth_client = AuthServiceClient::new(http, host_url).unwrap();

                self.clients
                    .insert(host.to_string(), (sync_client, auth_client));
                self.clients.get_mut(host).unwrap()
            }
        }
    }
}
