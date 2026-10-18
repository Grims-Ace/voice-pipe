// ============================================================================
// ============================================================================
// Manage proxy bots: the pool of free accounts, and a proxy session that
// joins voice as one user of the other platform, or as a group of users.
// ============================================================================
// ============================================================================

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use tracing::{info, warn};

use super::hub::Hub;
use crate::gateway::{
        Gateway, INTENT_GUILD_VOICE_STATES, INTENT_GUILDS, VoiceFlags,
};
use crate::platform::Platform;
use crate::rest::{NICKNAME_MAX, RestClient, Truncate, UserProfile};
use crate::voice::{VoiceConnection, VoiceTarget};

// ============================================================================
// Constants
// ============================================================================

// Proxies need only guild data and their own voice state.
const PROXY_INTENTS: u64 = INTENT_GUILDS | INTENT_GUILD_VOICE_STATES;
// Time limit for a proxy to log in and join voice.
const OPEN_TIMEOUT: Duration = Duration::from_secs(30);
// Time that a proxy waits after a failure before the pool gives it out again.
const HOLD_TIME: Duration = Duration::from_secs(60);

// ============================================================================
// Types
// ============================================================================

// What a proxy shows in the channel: one user of the other platform, or the
// group name when it speaks for many users.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProxyLook {
        User(String),
        Group,
}

// The avatar that a proxy has now. Avatar changes are slow and have strict
// rate limits, so a proxy changes its avatar only when it must.
#[derive(Clone, PartialEq, Eq)]
enum AvatarState {
        Unknown,
        Removed,
        User(String),
}

// The name and avatar of a proxy bot. Clones share the same state, so that
// a background task can change the look of a proxy that is in voice.
#[derive(Clone)]
pub struct ProxyProfile {
        _rest: RestClient,
        _botId: String,
        _nickname: Arc<Mutex<String>>,
        _avatar: Arc<Mutex<AvatarState>>,
}

// A proxy bot account.
pub struct ProxyAccount {
        _token: String,
        _idleName: Option<String>,
        _profile: ProxyProfile,
}

// The proxy accounts of one platform that are not in use.
pub struct ProxyPool {
        _free: VecDeque<ProxyAccount>,
        _held: Vec<(Instant, ProxyAccount)>,
        _total: usize,
}

// A proxy bot in a voice channel that looks like a user of the other
// platform, or like the group of users that it speaks for.
pub struct ProxySession {
        _account: ProxyAccount,
        _gateway: Gateway,
        _voice: VoiceConnection,
        _guildId: String,
        _voiceFlags: VoiceFlags,
}

// ============================================================================
// Proxy Profile
// ============================================================================

impl ProxyProfile {
        pub fn getNickname(&self) -> String {
                self._nickname.lock().unwrap().clone()
        }

        fn New(rest: RestClient, bot_id: &str) -> Self {
                Self {
                        _rest: rest,
                        _botId: bot_id.to_string(),
                        _nickname: Arc::new(Mutex::new(String::new())),
                        _avatar: Arc::new(Mutex::new(AvatarState::Unknown)),
                }
        }

        // Change the name and avatar of the proxy to `look`. Errors only
        // cause warnings, because a proxy with the wrong look still relays
        // audio.
        pub async fn Apply(
                &self,
                hub: &Hub,
                platform: Platform,
                look: &ProxyLook,
        ) {
                match look {
                        ProxyLook::User(user) => {
                                self.CopyUser(hub, platform, user).await
                        }
                        ProxyLook::Group => self.ShowGroup(hub, platform).await,
                }
        }

        // Copy the name and avatar of a user from the other platform.
        async fn CopyUser(
                &self,
                hub: &Hub,
                platform: Platform,
                user_id: &str,
        ) {
                let source = hub.getPlatform(platform.getOpposite());
                let guild_id = hub.getPlatform(platform).getGuildId();
                let profile = FetchUserProfile(hub, platform, user_id).await;
                let fallback = platform.getOpposite().getTitle();
                let nickname = profile
                        .as_ref()
                        .map(|p| p.getName().to_string())
                        .unwrap_or_else(|| format!("{fallback} User"));
                self.SetNickname(guild_id, &nickname).await;
                // Without a profile the avatar is not known, so keep the old
                // one and try again on the next change.
                let Some(profile) = profile else { return };
                let wanted = AvatarState::User(user_id.to_string());
                if *self._avatar.lock().unwrap() == wanted { return; }
                let (source, proxy) = (source.getRest(), &self._rest);
                if CopyAvatar(source, proxy, guild_id, &profile).await {
                        *self._avatar.lock().unwrap() = wanted;
                }
        }

        // Show the group name and remove the avatar of the last user.
        async fn ShowGroup(&self, hub: &Hub, platform: Platform) {
                let guild_id = hub.getPlatform(platform).getGuildId();
                let name = hub.getConfig().getGeneral().getOverflowName();
                self.SetNickname(guild_id, &Truncate(name, NICKNAME_MAX)).await;
                if *self._avatar.lock().unwrap() == AvatarState::Removed {
                        return;
                }
                let Err(e) = self._rest.SetAvatar(guild_id, None).await else {
                        *self._avatar.lock().unwrap() = AvatarState::Removed;
                        return;
                };
                warn!(bot = %self._botId, error = %e, "cannot remove avatar");
        }

        // Set the nickname in the guild and remember it for the status.
        async fn SetNickname(&self, guild_id: &str, nickname: &str) {
                *self._nickname.lock().unwrap() = nickname.to_string();
                let (rest, bot) = (&self._rest, &self._botId);
                let result =
                        rest.SetNickname(guild_id, bot, Some(nickname)).await;
                if let Err(e) = result {
                        warn!(bot = %bot, error = %e, "cannot set nickname");
                }
        }
}

// ============================================================================
// Proxy Account
// ============================================================================

impl ProxyAccount {
        pub fn getBotId(&self) -> &str { &self._profile._botId }

        // `idle_name` is the nickname of the proxy while it mirrors nobody.
        pub fn New(
                token: &str,
                bot_id: &str,
                rest: RestClient,
                idle_name: Option<String>,
        ) -> Self {
                Self {
                        _token: token.to_string(),
                        _idleName: idle_name
                                .map(|n| Truncate(&n, NICKNAME_MAX)),
                        _profile: ProxyProfile::New(rest, bot_id),
                }
        }

        // Set the idle nickname, so that a free proxy does not keep the
        // name of the last person or group that it mirrored.
        pub async fn SetIdleName(&self, guild_id: &str) {
                let name = self._idleName.as_deref();
                let (rest, bot) = (&self._profile._rest, self.getBotId());
                let result = rest.SetNickname(guild_id, bot, name).await;
                if let Err(e) = result {
                        warn!(bot = %bot, error = %e, "cannot set idle name");
                }
        }
}

// ============================================================================
// Proxy Pool
// ============================================================================

impl ProxyPool {
        pub fn getTotal(&self) -> usize { self._total }
        pub fn getFreeCount(&self) -> usize { self._free.len() }
        pub fn getHeldCount(&self) -> usize { self._held.len() }

        pub fn New(accounts: Vec<ProxyAccount>) -> Self {
                Self {
                        _total: accounts.len(),
                        _free: accounts.into(),
                        _held: Vec::new(),
                }
        }

        // Give out a free account, or None when all accounts are in use.
        pub fn Acquire(&mut self) -> Option<ProxyAccount> {
                self.ReleaseHeld();
                self._free.pop_front()
        }

        // Return the number of accounts that Acquire can give out now.
        pub fn CountAvailable(&mut self) -> usize {
                self.ReleaseHeld();
                self._free.len()
        }

        pub fn Release(&mut self, account: ProxyAccount) {
                self._free.push_back(account);
        }

        // Hold back an account that failed, so that a bad account does not
        // fail again and again.
        pub fn Hold(&mut self, account: ProxyAccount) {
                self._held.push((Instant::now() + HOLD_TIME, account));
        }

        fn ReleaseHeld(&mut self) {
                let now = Instant::now();
                let (ready, waiting): (Vec<_>, Vec<_>) =
                        std::mem::take(&mut self._held)
                                .into_iter()
                                .partition(|(at, _)| *at <= now);
                self._held = waiting;
                self._free
                        .extend(ready.into_iter().map(|(_, account)| account));
        }
}

// ============================================================================
// Proxy Session
// ============================================================================

impl ProxySession {
        pub fn getNickname(&self) -> String {
                self._account._profile.getNickname()
        }
        pub fn getProfile(&self) -> ProxyProfile {
                self._account._profile.clone()
        }
        pub fn getVoice(&self) -> &VoiceConnection { &self._voice }

        // Show a mute and deafen state on the proxy in `channel_id`, the
        // channel that it is in. Send it only when it changes.
        pub fn ShowVoiceFlags(&mut self, channel_id: &str, flags: VoiceFlags) {
                if flags == self._voiceFlags { return; }
                let gateway = &self._gateway;
                match gateway.SetVoiceFlags(&self._guildId, channel_id, flags) {
                        Ok(()) => self._voiceFlags = flags,
                        Err(e) => {
                                let bot = self._account.getBotId();
                                let error = format!("{e:#}");
                                warn!(%bot, %error, "cannot set mute state");
                        }
                }
        }

        // Give the proxy the look of `look`, log it in, and join the channel
        // of `target`. Give the account back on failure.
        #[allow(clippy::result_large_err)]
        pub async fn Open(
                hub: &Hub,
                account: ProxyAccount,
                target: &VoiceTarget,
                platform: Platform,
                look: &ProxyLook,
        ) -> Result<ProxySession, ProxyAccount> {
                account._profile.Apply(hub, platform, look).await;
                let side = hub.getPlatform(platform);
                let bot_id = account.getBotId();
                let label = format!("{platform} proxy {bot_id}");
                let gateway = Gateway::Connect(side.BuildGatewaySettings(
                        &label,
                        &account._token,
                        bot_id,
                        PROXY_INTENTS,
                ));
                let join = JoinVoice(&gateway, side.getGuildId(), target);
                let joined = tokio::time::timeout(OPEN_TIMEOUT, join).await;
                let error = match joined {
                        Ok(Ok(voice)) => {
                                let nickname = account._profile.getNickname();
                                info!(
                                        %platform,
                                        ?look,
                                        %nickname,
                                        "proxy joined voice",
                                );
                                return Ok(ProxySession {
                                        _account: account,
                                        _gateway: gateway,
                                        _voice: voice,
                                        _guildId: side.getGuildId().to_string(),
                                        _voiceFlags: VoiceFlags::default(),
                                });
                        }
                        Ok(Err(e)) => format!("{e:#}"),
                        Err(_) => "join timed out".to_string(),
                };
                Err(Fail(account, gateway, &error).await)
        }

        // Leave voice, log out, and set the idle name. Return the account
        // for the pool.
        pub async fn Close(self) -> ProxyAccount {
                self._voice.Close().await;
                self._gateway.LeaveVoice(&self._guildId);
                self._gateway.Close().await;
                self._account.SetIdleName(&self._guildId).await;
                self._account
        }
}

// ============================================================================
// Helper Functions
// ============================================================================

async fn JoinVoice(
        gateway: &Gateway,
        guild_id: &str,
        target: &VoiceTarget,
) -> Result<VoiceConnection> {
        gateway.WaitForGuild(guild_id, OPEN_TIMEOUT).await?;
        VoiceConnection::Join(gateway, target).await
}

async fn Fail(
        account: ProxyAccount,
        gateway: Gateway,
        reason: &str,
) -> ProxyAccount {
        let bot = account.getBotId();
        warn!(bot = %bot, %reason, "proxy could not join voice");
        gateway.Close().await;
        account
}

// Read the profile of a user on the other platform. Return None on failure.
async fn FetchUserProfile(
        hub: &Hub,
        platform: Platform,
        user_id: &str,
) -> Option<UserProfile> {
        let source = hub.getPlatform(platform.getOpposite());
        let rest = source.getRest();
        match rest.FetchProfile(source.getGuildId(), user_id).await {
                Ok(profile) => Some(profile),
                Err(e) => {
                        warn!(
                                user = %user_id,
                                error = %e,
                                "cannot read user profile",
                        );
                        None
                }
        }
}

// Set the guild avatar of the proxy to the avatar of the user, or remove it
// if the user has no avatar. Return true on success.
async fn CopyAvatar(
        source: &RestClient,
        proxy: &RestClient,
        guild_id: &str,
        profile: &UserProfile,
) -> bool {
        let image = match profile.getAvatarUrl() {
                Some(url) => match source.FetchImage(url).await {
                        Ok(image) => Some(image),
                        Err(e) => {
                                warn!(
                                        %url,
                                        error = %e,
                                        "cannot download avatar",
                                );
                                return false;
                        }
                },
                None => None,
        };
        let Err(e) = proxy.SetAvatar(guild_id, image.as_deref()).await else {
                return true;
        };
        warn!(
                platform = %proxy.getPlatform(),
                error = %e,
                "cannot set proxy avatar",
        );
        false
}
