//! Reconnection manager with exponential backoff.
//!
//! Handles automatic reconnection for enrolled clients when connection is lost.

use std::time::{Duration, Instant};
use tokio::time::sleep;
use rustynaut_common::config::ReconnectConfig;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ConnectionState {
    Disconnected,
    Connected,
    Reconnecting,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DisconnectReason {
    Intentional,  // User quit
    BrokerRestart,
    NetworkError,
    Timeout,
    Unknown,
}

pub struct ReconnectionManager {
    config: ReconnectConfig,
    attempt: u32,
    state: ConnectionState,
    connection_start: Option<Instant>,
    context: ReconnectContext,
}

#[derive(Clone, Debug)]
pub struct ReconnectContext {
    pub broker_addr: String,
    pub username: String,
    pub room: String,
}

impl ReconnectionManager {
    pub fn new(config: ReconnectConfig, context: ReconnectContext) -> Self {
        Self {
            config,
            attempt: 0,
            state: ConnectionState::Disconnected,
            connection_start: None,
            context,
        }
    }

    pub fn set_connected(&mut self) {
        self.state = ConnectionState::Connected;
        self.connection_start = Some(Instant::now());
        self.attempt = 0;
    }

    pub fn set_disconnected(&mut self) {
        self.state = ConnectionState::Disconnected;
        self.connection_start = None;
    }

    pub fn state(&self) -> ConnectionState {
        self.state
    }

    pub fn connection_duration(&self) -> Option<Duration> {
        self.connection_start.map(|start| start.elapsed())
    }

    pub fn should_reconnect(&self, reason: DisconnectReason) -> bool {
        if !self.config.enabled {
            return false;
        }

        if self.attempt >= self.config.max_attempts {
            return false;
        }

        if reason == DisconnectReason::Intentional {
            return false;
        }

        if let Some(duration) = self.connection_duration() {
            if duration < Duration::from_secs(self.config.min_connection_seconds) {
                return false;
            }
        }

        true
    }

    pub fn calculate_backoff(&self) -> Duration {
        let delay = self.config.base_delay_seconds * 2u64.pow(self.attempt);
        let delay = std::cmp::min(delay, self.config.max_delay_seconds);
        Duration::from_secs(delay)
    }

    pub async fn wait_backoff(&mut self) {
        let delay = self.calculate_backoff();
        self.attempt += 1;
        self.state = ConnectionState::Reconnecting;
        sleep(delay).await;
    }

    pub fn start_backoff(&mut self) -> Duration {
        let delay = self.calculate_backoff();
        self.attempt += 1;
        self.state = ConnectionState::Reconnecting;
        delay
    }

    pub fn reset(&mut self) {
        self.attempt = 0;
    }

    pub fn attempt(&self) -> u32 {
        self.attempt
    }

    pub fn max_attempts(&self) -> u32 {
        self.config.max_attempts
    }

    pub fn context(&self) -> &ReconnectContext {
        &self.context
    }

    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> ReconnectConfig {
        ReconnectConfig {
            enabled: true,
            max_attempts: 3,
            base_delay_seconds: 1,
            max_delay_seconds: 8,
            min_connection_seconds: 5,
        }
    }

    fn test_context() -> ReconnectContext {
        ReconnectContext {
            broker_addr: "127.0.0.1:4242".to_string(),
            username: "testuser".to_string(),
            room: "lobby".to_string(),
        }
    }

    #[test]
    fn test_should_not_reconnect_when_disabled() {
        let mut config = test_config();
        config.enabled = false;
        let manager = ReconnectionManager::new(config, test_context());
        
        assert!(!manager.should_reconnect(DisconnectReason::NetworkError));
    }

    #[test]
    fn test_should_not_reconnect_on_intentional() {
        let manager = ReconnectionManager::new(test_config(), test_context());
        
        assert!(!manager.should_reconnect(DisconnectReason::Intentional));
    }

    #[test]
    fn test_should_reconnect_on_network_error() {
        let manager = ReconnectionManager::new(test_config(), test_context());
        
        assert!(manager.should_reconnect(DisconnectReason::NetworkError));
    }

    #[test]
    fn test_backoff_increases() {
        let mut manager = ReconnectionManager::new(test_config(), test_context());
        
        assert_eq!(manager.calculate_backoff(), Duration::from_secs(1));
        
        manager.attempt += 1;
        assert_eq!(manager.calculate_backoff(), Duration::from_secs(2));
        
        manager.attempt += 1;
        assert_eq!(manager.calculate_backoff(), Duration::from_secs(4));
    }

    #[test]
    fn test_backoff_capped_at_max() {
        let mut config = test_config();
        config.base_delay_seconds = 4;
        
        let mut manager = ReconnectionManager::new(config, test_context());
        
        // 4 * 2^0 = 4s
        assert_eq!(manager.calculate_backoff(), Duration::from_secs(4));
        
        manager.attempt += 1;
        // 4 * 2^1 = 8s, capped at max 8
        assert_eq!(manager.calculate_backoff(), Duration::from_secs(8));
        
        manager.attempt += 1;
        // 4 * 2^2 = 16s, capped at max 8
        assert_eq!(manager.calculate_backoff(), Duration::from_secs(8));
    }

    #[test]
    fn test_reset_clears_attempt() {
        let mut manager = ReconnectionManager::new(test_config(), test_context());
        manager.attempt = 2;
        
        manager.reset();
        
        assert_eq!(manager.attempt, 0);
    }

    #[test]
    fn test_set_connected_resets_attempt() {
        let mut manager = ReconnectionManager::new(test_config(), test_context());
        manager.attempt = 2;
        
        manager.set_connected();
        
        assert_eq!(manager.attempt, 0);
        assert_eq!(manager.state(), ConnectionState::Connected);
    }
}
