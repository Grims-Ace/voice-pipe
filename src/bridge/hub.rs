// ============================================================================
// ============================================================================
// Hold the state that all links share: the main bot of each platform, its
// gateway, the proxy pool, and the on/off switch of the bridge. The switch
// state is saved, so that it stays the same after a restart.
// ============================================================================
// ============================================================================

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use futures_util::future::join_all;
use tracing::{error, info};

use super::link::LinkKey;
use super::proxy::{ProxyAccount, ProxyPool};
use super::saved;
use crate::config::{Config, GeneralSettings, PlatformSettings};
use crate::gateway::{
        Gateway, GatewaySettings, INTENT_GUILD_MESSAGES,
        INTENT_GUILD_VOICE_STATES, INTENT_GUILDS, INTENT_MESSAGE_CONTENT,
        VoiceFlags,
};
use crate::platform::Platform;
use crate::rest::RestClient;
use crate::voice::AudioTuning;

// Time limit for the main gateway to send the guild after login.
const GUILD_TIMEOUT: Duration = Duration::from_secs(30);

// ============================================================================
// Types
// ============================================================================

// The main bot, its connections, and the proxy pool of one platform.
pub struct PlatformHub {
        _platform: Platform,
        _guildId: String,
        _mainBotId: String,
        _gatewayUrl: String,
        _hostHeader: Option<String>,
        _livekitUrl: Option<String>,
        _gateway: Gateway,
        _rest: RestClient,
        _botIds: Arc<HashSet<String>>,
        _proxies: Mutex<ProxyPool>,
        _mainLink: Mutex<Option<LinkKey>>,
}

// The shared state of the whole bridge.
pub struct Hub {
        _config: Arc<Config>,
        _tuning: AudioTuning,
        _discord: PlatformHub,
        _fluxer: PlatformHub,
        _isEnabled: AtomicBool,
}

// ============================================================================
// Platform Hub
// ============================================================================

impl PlatformHub {
        pub fn getGuildId(&self) -> &str { &self._guildId }
        pub fn getMainBotId(&self) -> &str { &self._mainBotId }
        pub fn getLivekitUrl(&self) -> Option<&str> {
                self._livekitUrl.as_deref()
        }
        pub fn getGateway(&self) -> &Gateway { &self._gateway }
        pub fn getRest(&self) -> &RestClient { &self._rest }

        // Return the IDs of the main bot and all proxies of this platform.
        pub fn getBotIds(&self) -> Arc<HashSet<String>> { self._botIds.clone() }

        // Return the link that the main bot is in, if any.
        pub fn getMainLink(&self) -> Option<LinkKey> {
                self._mainLink.lock().unwrap().clone()
        }

        // Return (free, held, total) proxy counts.
        pub fn getProxyCounts(&self) -> (usize, usize, usize) {
                let pool = self._proxies.lock().unwrap();
                (pool.getFreeCount(), pool.getHeldCount(), pool.getTotal())
        }

        // Return the people in a voice channel, without the bridge bots,
        // with their mute and deafen state.
        pub fn getHumans(
                &self,
                channel_id: &str,
        ) -> HashMap<String, VoiceFlags> {
                let mut users = self
                        ._gateway
                        .getChannelUsers(&self._guildId, channel_id);
                users.retain(|u, _| !self._botIds.contains(u));
                users
        }

        // Log in the main bot and all proxy accounts of a platform.
        async fn Start(
                platform: Platform,
                config: &Config,
                http: &reqwest::Client,
        ) -> Result<Self> {
                let settings = config.getPlatform(platform);
                let rest = RestClient::New(
                        http,
                        platform,
                        settings,
                        settings.getMainToken(),
                );
                // The cause can be the token or the API URL, so name both.
                let main_id = rest.FetchSelfId().await.with_context(|| {
                        format!("{platform} login failed, check token, api_url")
                })?;
                let tokens = settings.getProxyTokens();
                let general = config.getGeneral();
                let proxies =
                        FetchProxies(&rest, tokens, &main_id, general).await;
                let guild_id = settings.getGuildId();
                join_all(proxies.iter().map(|p| p.SetIdleName(guild_id))).await;
                let mut bot_ids: HashSet<String> = proxies
                        .iter()
                        .map(|p| p.getBotId().to_string())
                        .collect();
                bot_ids.insert(main_id.clone());
                info!(
                        %platform,
                        bot = %main_id,
                        proxies = proxies.len(),
                        "bots logged in",
                );
                let gateway = Gateway::Connect(BuildMainGatewaySettings(
                        platform, settings, &main_id,
                ));
                gateway.WaitForGuild(settings.getGuildId(), GUILD_TIMEOUT)
                        .await?;
                Ok(Self {
                        _platform: platform,
                        _guildId: settings.getGuildId().to_string(),
                        _mainBotId: main_id,
                        _gatewayUrl: settings.getGatewayUrl().to_string(),
                        _hostHeader: settings.getHostHeader().map(String::from),
                        _livekitUrl: settings.getLivekitUrl().map(String::from),
                        _gateway: gateway,
                        _rest: rest,
                        _botIds: Arc::new(bot_ids),
                        _proxies: Mutex::new(ProxyPool::New(proxies)),
                        _mainLink: Mutex::new(None),
                })
        }

        // Make the gateway settings for another bot on this platform.
        pub fn BuildGatewaySettings(
                &self,
                label: &str,
                token: &str,
                bot_id: &str,
                intents: u64,
        ) -> GatewaySettings {
                GatewaySettings::New(
                        label,
                        self._platform,
                        &self._gatewayUrl,
                        token,
                        intents,
                        bot_id,
                        self._hostHeader.as_deref(),
                )
        }

        // Reserve the main bot for a link. The main bot can be in only one
        // voice channel per guild, so only one link can use it.
        pub fn ClaimMain(&self, key: &LinkKey) -> bool {
                let mut slot = self._mainLink.lock().unwrap();
                if slot.is_some() { return false; }
                *slot = Some(key.clone());
                true
        }

        pub fn ReleaseMain(&self, key: &LinkKey) {
                let mut slot = self._mainLink.lock().unwrap();
                if slot.as_ref() == Some(key) { *slot = None; }
        }

        // Return the number of proxies that are free to use now.
        pub fn CountFreeProxies(&self) -> usize {
                self._proxies.lock().unwrap().CountAvailable()
        }

        pub fn AcquireProxy(&self) -> Option<ProxyAccount> {
                self._proxies.lock().unwrap().Acquire()
        }
        pub fn ReleaseProxy(&self, account: ProxyAccount) {
                self._proxies.lock().unwrap().Release(account)
        }
        pub fn HoldProxy(&self, account: ProxyAccount) {
                self._proxies.lock().unwrap().Hold(account)
        }
}

// ============================================================================
// Hub
// ============================================================================

impl Hub {
        pub fn getConfig(&self) -> &Config { &self._config }
        pub fn getTuning(&self) -> AudioTuning { self._tuning }
        pub fn isEnabled(&self) -> bool {
                self._isEnabled.load(Ordering::Relaxed)
        }

        pub fn getPlatform(&self, platform: Platform) -> &PlatformHub {
                match platform {
                        Platform::Discord => &self._discord,
                        Platform::Fluxer => &self._fluxer,
                }
        }

        // Turn the bridge on or off, and save the state for a restart.
        pub fn SetEnabled(&self, is_enabled: bool) {
                self._isEnabled.store(is_enabled, Ordering::Relaxed);
                info!(is_enabled, "bridge switched");
                let general = self._config.getGeneral();
                let Some(path) = general.getStateFile() else { return };
                saved::WriteEnabled(&path, is_enabled);
        }

        // Log in all bots on both platforms.
        pub async fn Start(config: Arc<Config>) -> Result<Arc<Hub>> {
                let http = reqwest::Client::builder().build()?;
                let (discord, fluxer) = tokio::try_join!(
                        PlatformHub::Start(Platform::Discord, &config, &http),
                        PlatformHub::Start(Platform::Fluxer, &config, &http),
                )?;
                let general = config.getGeneral();
                let is_enabled = FindStartState(general);
                Ok(Arc::new(Hub {
                        _tuning: AudioTuning::New(general.getSilenceHangover()),
                        _isEnabled: AtomicBool::new(is_enabled),
                        _config: config,
                        _discord: discord,
                        _fluxer: fluxer,
                }))
        }

        // Log out the main bots.
        pub async fn Close(&self) {
                tokio::join!(
                        self._discord._gateway.Close(),
                        self._fluxer._gateway.Close()
                );
        }
}

// ============================================================================
// Helper Functions
// ============================================================================

// Use the saved switch state. Use start_enabled when no state was saved.
fn FindStartState(general: &GeneralSettings) -> bool {
        let saved = general.getStateFile().and_then(|p| saved::ReadEnabled(&p));
        match saved {
                Some(is_enabled) => {
                        info!(is_enabled, "saved switch state loaded");
                        is_enabled
                }
                None => general.isStartEnabled(),
        }
}

// The Discord main bot reads chat messages for the commands.
fn BuildMainGatewaySettings(
        platform: Platform,
        settings: &PlatformSettings,
        bot_id: &str,
) -> GatewaySettings {
        let mut intents = INTENT_GUILDS | INTENT_GUILD_VOICE_STATES;
        if platform == Platform::Discord {
                intents |= INTENT_GUILD_MESSAGES | INTENT_MESSAGE_CONTENT;
        }
        GatewaySettings::New(
                &format!("{platform} main"),
                platform,
                settings.getGatewayUrl(),
                settings.getMainToken(),
                intents,
                bot_id,
                settings.getHostHeader(),
        )
}

// Find the bot ID of each proxy token. Skip tokens that do not work.
async fn FetchProxies(
        rest: &RestClient,
        tokens: &[String],
        main_id: &str,
        general: &GeneralSettings,
) -> Vec<ProxyAccount> {
        let lookups = tokens.iter().map(|token| async move {
                let client = rest.WithToken(token);
                (token, client.FetchSelfId().await, client)
        });
        let mut accounts = Vec::new();
        for (i, (token, result, client)) in
                join_all(lookups).await.into_iter().enumerate()
        {
                match result {
                        Ok(id) if id == main_id => {
                                error!(index = i, "proxy token is the main bot")
                        }
                        Ok(id) => {
                                let name = general.getProxyIdleName(i + 1);
                                accounts.push(ProxyAccount::New(
                                        token, &id, client, name,
                                ));
                        }
                        Err(e) => {
                                error!(
                                        platform = %rest.getPlatform(),
                                        index = i,
                                        error = %e,
                                        "proxy token does not work",
                                )
                        }
                }
        }
        accounts
}
