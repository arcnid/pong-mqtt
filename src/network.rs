use rumqttc::{Client, MqttOptions, QoS};
use serde::{Deserialize, Serialize};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Message types (shared between client and server)
// ---------------------------------------------------------------------------

/// Sent by each client → server: "my paddle is at this Y position"
/// y is in physics/court units (0..COURT_HEIGHT), matching the server's coordinate space.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PaddleMsg {
    pub y: f32,
    pub timestamp: u64,
}

/// Sent by server → clients: authoritative ball position + velocity
/// Field names match the TypeScript server exactly (camelCase/snake_case as published).
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct BallMsg {
    pub x: f32,
    pub y: f32,
    pub dx: f32,
    pub dy: f32,
    pub timestamp: u64,
}

/// Sent by server → clients: scores and game lifecycle
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct StateMsg {
    #[serde(rename = "p1Score")]
    pub p1_score: u32,
    #[serde(rename = "p2Score")]
    pub p2_score: u32,
    pub status: GameStatus,
    pub timestamp: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum GameStatus {
    Waiting,
    Playing,
    Ended,
}

/// Join notification sent by client → server on connect
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct JoinMsg {
    pub player: u8, // 1 or 2
    pub timestamp: u64,
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

    pub fn serve(&self) -> String {
        format!("pong/game/{}/serve", self.game_id)
    }

    pub fn restart(&self) -> String {
        format!("pong/game/{}/restart", self.game_id)
    }

    pub fn ready(&self) -> String {
        format!("pong/game/{}/ready", self.game_id)
    }
}

// ---------------------------------------------------------------------------
// Events the network thread sends back to the game loop
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum NetworkEvent {
    /// Opponent paddle moved to this Y (physics/court units)
    OpponentPaddle(f32),
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
    /// Game loop sends its own paddle Y (physics units) here to be published
    pub paddle_tx: mpsc::SyncSender<f32>,
    /// Game loop sends () here to publish a serve message
    pub serve_tx: mpsc::SyncSender<()>,
    /// Game loop sends () here to publish a restart message (ignored unless game is ended)
    pub restart_tx: mpsc::SyncSender<()>,
    /// Game loop sends () here to publish a ready message (post-game restart coordination)
    pub ready_tx: mpsc::SyncSender<()>,
}

// ---------------------------------------------------------------------------
// Spawn the MQTT thread
// ---------------------------------------------------------------------------

pub fn connect(config: NetworkConfig) -> NetworkHandle {
    let (event_tx, event_rx) = mpsc::channel::<NetworkEvent>();
    let (paddle_tx, paddle_rx) = mpsc::sync_channel::<f32>(32);
    let (serve_tx, serve_rx) = mpsc::sync_channel::<()>(4);
    let (restart_tx, restart_rx) = mpsc::sync_channel::<()>(4);
    let (ready_tx, ready_rx) = mpsc::sync_channel::<()>(4);

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
            timestamp: now_ms(),
        })
        .unwrap_or_default();
        client.publish(topics.join(), QoS::AtMostOnce, false, join_payload).ok();

        // Send a restart request immediately after join.
        // If the game is in 'ended' state (stale session on server), this resets it.
        // If the game is 'waiting' or 'playing', the server ignores it.
        #[derive(serde::Serialize)]
        struct RestartMsg { timestamp: u64 }
        if let Ok(payload) = serde_json::to_vec(&RestartMsg { timestamp: now_ms() }) {
            client.publish(topics.restart(), QoS::AtMostOnce, false, payload).ok();
        }

        // Spawn a sub-thread to forward outgoing paddle positions (physics units)
        let publish_client = client.clone();
        let my_paddle_topic = my_paddle.clone();
        thread::spawn(move || {
            while let Ok(y) = paddle_rx.recv() {
                let msg = PaddleMsg {
                    y,
                    timestamp: now_ms(),
                };
                if let Ok(payload) = serde_json::to_vec(&msg) {
                    publish_client
                        .publish(&my_paddle_topic, QoS::AtMostOnce, true, payload)
                        .ok();
                }
            }
        });

        // Spawn a sub-thread to forward serve signals
        let serve_client = client.clone();
        let serve_topic = topics.serve();
        let player_num = config.player;
        thread::spawn(move || {
            while let Ok(()) = serve_rx.recv() {
                #[derive(serde::Serialize)]
                struct ServeMsg { player: u8, timestamp: u64 }
                if let Ok(payload) = serde_json::to_vec(&ServeMsg { player: player_num, timestamp: now_ms() }) {
                    serve_client.publish(&serve_topic, QoS::AtMostOnce, false, payload).ok();
                }
            }
        });

        // Spawn a sub-thread to forward restart signals
        let restart_client = client.clone();
        let restart_topic = topics.restart();
        thread::spawn(move || {
            while let Ok(()) = restart_rx.recv() {
                #[derive(serde::Serialize)]
                struct RestartMsg2 { timestamp: u64 }
                if let Ok(payload) = serde_json::to_vec(&RestartMsg2 { timestamp: now_ms() }) {
                    restart_client.publish(&restart_topic, QoS::AtMostOnce, false, payload).ok();
                }
            }
        });

        // Spawn a sub-thread to forward ready signals (post-game restart coordination)
        let ready_client = client.clone();
        let ready_topic = topics.ready();
        let player_num2 = config.player;
        thread::spawn(move || {
            while let Ok(()) = ready_rx.recv() {
                #[derive(serde::Serialize)]
                struct ReadyMsg { player: u8, timestamp: u64 }
                if let Ok(payload) = serde_json::to_vec(&ReadyMsg { player: player_num2, timestamp: now_ms() }) {
                    ready_client.publish(&ready_topic, QoS::AtMostOnce, false, payload).ok();
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
                            event_tx.send(NetworkEvent::OpponentPaddle(p.y as f32)).ok();
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
        serve_tx,
        restart_tx,
        ready_tx,
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
