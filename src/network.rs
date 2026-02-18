use rumqttc::{Client, MqttOptions, QoS};
use serde::{Deserialize, Serialize};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Message types (shared between client and server)
// ---------------------------------------------------------------------------

/// Sent by each client → server: "my paddle is at this Y position"
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PaddleMsg {
    pub y: u16,
    pub timestamp_ms: u64,
}

/// Sent by server → clients: authoritative ball position + velocity
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct BallMsg {
    pub x: u16,
    pub y: u16,
    pub vx: i8,
    pub vy: i8,
    pub timestamp_ms: u64,
}

/// Sent by server → clients: scores and game lifecycle
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct StateMsg {
    pub p1_score: u32,
    pub p2_score: u32,
    pub status: GameStatus,
    pub timestamp_ms: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum GameStatus {
    Waiting,
    Playing,
    Scored,
    GameOver,
}

/// Join notification sent by client → server on connect
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct JoinMsg {
    pub player: u8, // 1 or 2
    pub timestamp_ms: u64,
}

// ---------------------------------------------------------------------------
// Topic helpers
// ---------------------------------------------------------------------------

pub struct Topics {
    pub game_id: String,
}

impl Topics {
    pub fn new(game_id: &str) -> Self {
        Self {
            game_id: game_id.to_string(),
        }
    }

    pub fn p1_paddle(&self) -> String {
        format!("pong/game/{}/p1/paddle", self.game_id)
    }

    pub fn p2_paddle(&self) -> String {
        format!("pong/game/{}/p2/paddle", self.game_id)
    }

    pub fn ball(&self) -> String {
        format!("pong/game/{}/ball", self.game_id)
    }

    pub fn state(&self) -> String {
        format!("pong/game/{}/state", self.game_id)
    }

    pub fn join(&self) -> String {
        format!("pong/game/{}/join", self.game_id)
    }
}

// ---------------------------------------------------------------------------
// Events the network thread sends back to the game loop
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum NetworkEvent {
    /// Opponent paddle moved to this Y
    OpponentPaddle(u16),
    /// Server published authoritative ball state
    BallUpdate(BallMsg),
    /// Server published scores / status
    StateUpdate(StateMsg),
    /// MQTT connection established
    Connected,
    /// MQTT connection lost
    Disconnected,
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

pub struct NetworkConfig {
    pub broker_host: String,
    pub broker_port: u16,
    pub game_id: String,
    pub player: u8, // 1 or 2
    pub username: Option<String>,
    pub password: Option<String>,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            broker_host: "3.141.116.27".to_string(),
            broker_port: 1883,
            game_id: "demo".to_string(),
            player: 1,
            username: Some("raptor".to_string()),
            password: Some("raptorMQTT2025".to_string()),
        }
    }
}

// ---------------------------------------------------------------------------
// Network handle - returned to the game loop
// ---------------------------------------------------------------------------

pub struct NetworkHandle {
    /// Game loop reads events from here
    pub rx: mpsc::Receiver<NetworkEvent>,
    /// Game loop sends its own paddle Y here to be published
    pub paddle_tx: mpsc::SyncSender<u16>,
}

// ---------------------------------------------------------------------------
// Spawn the MQTT thread
// ---------------------------------------------------------------------------

pub fn connect(config: NetworkConfig) -> NetworkHandle {
    let (event_tx, event_rx) = mpsc::channel::<NetworkEvent>();
    let (paddle_tx, paddle_rx) = mpsc::sync_channel::<u16>(32);

    thread::spawn(move || {
        let topics = Topics::new(&config.game_id);
        let client_id = format!("rust-pong-p{}-{}", config.player, &config.game_id[..4.min(config.game_id.len())]);

        let mut mqttoptions = MqttOptions::new(client_id, &config.broker_host, config.broker_port);
        mqttoptions.set_keep_alive(Duration::from_secs(5));

        if let (Some(user), Some(pass)) = (config.username, config.password) {
            mqttoptions.set_credentials(user, pass);
        }

        let (client, mut connection) = Client::new(mqttoptions, 64);

        // Subscribe to the topics we care about
        let my_paddle = if config.player == 1 {
            topics.p1_paddle()
        } else {
            topics.p2_paddle()
        };
        let opponent_paddle = if config.player == 1 {
            topics.p2_paddle()
        } else {
            topics.p1_paddle()
        };

        // We subscribe to: opponent paddle, ball, state
        client.subscribe(&opponent_paddle, QoS::AtMostOnce).ok();
        client.subscribe(topics.ball(), QoS::AtMostOnce).ok();
        client.subscribe(topics.state(), QoS::AtMostOnce).ok();

        // Announce join
        let join_payload = serde_json::to_vec(&JoinMsg {
            player: config.player,
            timestamp_ms: now_ms(),
        })
        .unwrap_or_default();
        client.publish(topics.join(), QoS::AtMostOnce, false, join_payload).ok();

        // Spawn a sub-thread to forward outgoing paddle positions
        let publish_client = client.clone();
        let my_paddle_topic = my_paddle.clone();
        thread::spawn(move || {
            while let Ok(y) = paddle_rx.recv() {
                let msg = PaddleMsg {
                    y,
                    timestamp_ms: now_ms(),
                };
                if let Ok(payload) = serde_json::to_vec(&msg) {
                    publish_client
                        .publish(&my_paddle_topic, QoS::AtMostOnce, false, payload)
                        .ok();
                }
            }
        });

        // Main event loop for incoming MQTT messages
        for notification in connection.iter() {
            match notification {
                Ok(rumqttc::Event::Incoming(rumqttc::Packet::ConnAck(_))) => {
                    event_tx.send(NetworkEvent::Connected).ok();
                }
                Ok(rumqttc::Event::Incoming(rumqttc::Packet::Publish(msg))) => {
                    let t = &msg.topic;

                    if *t == opponent_paddle {
                        if let Ok(p) = serde_json::from_slice::<PaddleMsg>(&msg.payload) {
                            event_tx.send(NetworkEvent::OpponentPaddle(p.y)).ok();
                        }
                    } else if *t == topics.ball() {
                        if let Ok(b) = serde_json::from_slice::<BallMsg>(&msg.payload) {
                            event_tx.send(NetworkEvent::BallUpdate(b)).ok();
                        }
                    } else if *t == topics.state() {
                        if let Ok(s) = serde_json::from_slice::<StateMsg>(&msg.payload) {
                            event_tx.send(NetworkEvent::StateUpdate(s)).ok();
                        }
                    }
                }
                Err(_) => {
                    event_tx.send(NetworkEvent::Disconnected).ok();
                    break;
                }
                _ => {}
            }
        }
    });

    NetworkHandle {
        rx: event_rx,
        paddle_tx,
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
