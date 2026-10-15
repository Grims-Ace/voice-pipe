// ============================================================================
// ============================================================================
// Manage proxy bots: the pool of free accounts, and a proxy session that
// copies the name and avatar of one user and joins voice as that user.
// ============================================================================
// ============================================================================

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use anyhow::Result;
use tracing::{info, warn};

use super::hub::Hub;
use crate::gateway::{Gateway, INTENT_GUILD_VOICE_STATES, INTENT_GUILDS};
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
const REST_TIME: Duration = Duration::from_secs(60);

// ============================================================================
// Types
// ============================================================================

// A proxy bot account. It remembers whose avatar it has, because avatar
// changes are slow and have strict rate limits.
pub struct ProxyAccount {
        _token: String,
        _botId: String,
        _rest: RestClient,
        _idleName: Option<String>,
        _avatarUserId: Option<String>,
}

// The proxy accounts of one platform that are not in use.
pub struct ProxyPool {
        _free: VecDeque<ProxyAccount>,
        _resting: Vec<(Instant, ProxyAccount)>,
        _total: usize,
}

// A proxy bot in a voice channel that looks like a user of the other
// platform.
pub struct ProxySession {
        _account: ProxyAccount,
        _gateway: Gateway,
        _voice: VoiceConnection,
        _guildId: String,
        _nickname: String,
}

// ============================================================================
// Proxy Account
// ============================================================================

impl ProxyAccount {
        pub fn getBotId(&self) -> &str { &self._botId }

        // `idle_name` is the nickname of the proxy while it mirrors nobody.
        pub fn New(
                token: &str,
                bot_id: &str,
                rest: RestClient,
                idle_name: Option<String>,
        ) -> Self {
                Self {
                        _token: token.to_string(),
                        _botId: bot_id.to_string(),
                        _rest: rest,
                        _idleName: idle_name
                                .map(|n| Truncate(&n, NICKNAME_MAX)),
                        _avatarUserId: None,
                }
        }

        // Set the idle nickname, so that a free proxy does not keep the
        // name of the last person that it mirrored.
        pub async fn SetIdleName(&self, guild_id: &str) {
                let name = self._idleName.as_deref();
                let (rest, bot) = (&self._rest, &self._botId);
                let result = rest.SetNickname(guild_id, bot, name).await;
                if let Err(e) = result {
                        warn!(
                                bot = %self._botId,
                                error = %e,
                                "cannot set idle name",
                        );
                }
        }
}

// ============================================================================
// Proxy Pool
// ============================================================================

impl ProxyPool {
        pub fn getTotal(&self) -> usize { self._total }
        pub fn getFreeCount(&self) -> usize {
                self._free.len() + self._resting.len()
        }

        pub fn New(accounts: Vec<ProxyAccount>) -> Self {
                Self {
                        _total: accounts.len(),
                        _free: accounts.into(),
                        _resting: Vec::new(),
                }
        }

        // Give out a free account, or None when all accounts are in use.
        pub fn Acquire(&mut self) -> Option<ProxyAccount> {
                self.WakeRested();
                self._free.pop_front()
        }

        pub fn Return(&mut self, account: ProxyAccount) {
                self._free.push_back(account);
        }

        // Hold back an account that failed, so that a bad account does not
        // fail again and again.
        pub fn Rest(&mut self, account: ProxyAccount) {
                self._resting.push((Instant::now() + REST_TIME, account));
        }

        fn WakeRested(&mut self) {
                let now = Instant::now();
                let (ready, waiting): (Vec<_>, Vec<_>) =
                        std::mem::take(&mut self._resting)
                                .into_iter()
                                .partition(|(at, _)| *at <= now);
                self._resting = waiting;
                self._free
                        .extend(ready.into_iter().map(|(_, account)| account));
        }
}

// ============================================================================
// Proxy Session
// ============================================================================

impl ProxySession {
        pub fn getNickname(&self) -> &str { &self._nickname }
        pub fn getVoice(&self) -> &VoiceConnection { &self._voice }

        // Copy the name and avatar of `user_id` to the proxy, log it in, and
        // join the channel of `target`. Give the account back on failure.
        #[allow(clippy::result_large_err)]
        pub async fn Open(
                hub: &Hub,
                mut account: ProxyAccount,
                target: &VoiceTarget,
                platform: Platform,
                user_id: &str,
        ) -> Result<ProxySession, ProxyAccount> {
                let nickname =
                        Dress(hub, &mut account, platform, user_id).await;
                let side = hub.getPlatform(platform);
                let label = format!("{platform} proxy {}", account._botId);
                let gateway = Gateway::Connect(side.BuildGatewaySettings(
                        &label,
                        &account._token,
                        &account._botId,
                        PROXY_INTENTS,
                ));
                let join = JoinVoice(&gateway, side.getGuildId(), target);
                match tokio::time::timeout(OPEN_TIMEOUT, join).await {
                        Ok(Ok(voice)) => {
                                info!(
                                        %platform,
                                        user = %user_id,
                                        %nickname,
                                        "proxy joined voice",
                                );
                                Ok(ProxySession {
                                        _account: account,
                                        _gateway: gateway,
                                        _voice: voice,
                                        _guildId: side.getGuildId().to_string(),
                                        _nickname: nickname,
                                })
                        }
                        Ok(Err(e)) => {
                                Err(Fail(account, gateway, &format!("{e:#}"))
                                        .await)
                        }
                        Err(_) => {
                                Err(Fail(account, gateway, "join timed out")
                                        .await)
                        }
                }
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
        warn!(bot = %account._botId, %reason, "proxy could not join voice");
        gateway.Close().await;
        account
}

// Copy the name and avatar of a user from the other platform to the proxy.
// Return the nickname. Errors only cause warnings, because a proxy with the
// wrong name still relays audio.
async fn Dress(
        hub: &Hub,
        account: &mut ProxyAccount,
        platform: Platform,
        user_id: &str,
) -> String {
        let source = hub.getPlatform(platform.getOpposite());
        let guild_id = hub.getPlatform(platform).getGuildId();
        let profile = match source
                .getRest()
                .FetchProfile(source.getGuildId(), user_id)
                .await
        {
                Ok(profile) => Some(profile),
                Err(e) => {
                        warn!(
                                user = %user_id,
                                error = %e,
                                "cannot read user profile",
                        );
                        None
                }
        };
        let nickname = profile
                .as_ref()
                .map(|p| p.getName().to_string())
                .unwrap_or_else(|| {
                        format!("{} User", platform.getOpposite().getTitle())
                });
        if let Err(e) = account
                ._rest
                .SetNickname(guild_id, &account._botId, Some(&nickname))
                .await
        {
                warn!(
                        bot = %account._botId,
                        error = %e,
                        "cannot set proxy nickname",
                );
        }
        if account._avatarUserId.as_deref() != Some(user_id) {
                CopyAvatar(
                        source.getRest(),
                        &account._rest,
                        guild_id,
                        profile.as_ref(),
                )
                .await;
                account._avatarUserId = Some(user_id.to_string());
        }
        nickname
}

// Set the guild avatar of the proxy to the avatar of the user, or remove it
// if the user has no avatar.
async fn CopyAvatar(
        source: &RestClient,
        proxy: &RestClient,
        guild_id: &str,
        profile: Option<&UserProfile>,
) {
        let image = match profile.and_then(|p| p.getAvatarUrl()) {
                Some(url) => match source.FetchImage(url).await {
                        Ok(image) => Some(image),
                        Err(e) => {
                                warn!(
                                        %url,
                                        error = %e,
                                        "cannot download avatar",
                                );
                                return;
                        }
                },
                None => None,
        };
        if let Err(e) = proxy.SetAvatar(guild_id, image.as_deref()).await {
                warn!(
                        platform = %proxy.getPlatform(),
                        error = %e,
                        "cannot set proxy avatar",
                );
        }
}
