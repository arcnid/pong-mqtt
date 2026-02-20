# MQTT Pong 🏓

Real-time multiplayer pong running in your terminal, powered by MQTT and playable over the internet.

> **Forked from:** [terminal.pong](https://github.com/IshmamR/terminal.pong) by IshmamR
> Extended with cloud multiplayer using MQTT messaging and server-authoritative physics.

## 🎥 Demo

*[Video demo coming soon]*

---

## ✨ Features

- 🌐 **Cloud multiplayer** — play with anyone, anywhere over the internet
- 🎮 **Server-authoritative physics** — no lag, no desync, just smooth 60fps gameplay
- 🔄 **Ready-up restart system** — both players press Space to start a new game
- 🏆 **Game over overlay** — shows winner and final score
- 🎨 **Multiple color themes** — Monokai, Solarized, Dracula, Gruvbox, Nord, OneDark, High Contrast
- 📡 **MQTT messaging** — lightweight pub/sub protocol for real-time coordination
- 🚀 **Written in Rust** — terminal UI built with [`ratatui`](https://crates.io/crates/ratatui)

---

## 🧑‍💻 Getting Started

### Requirements

- Rust (latest stable recommended)
- A terminal that supports ANSI escape codes (most modern terminals)

### Installation

Clone and build:

```bash
git clone https://github.com/arcnid/pong-mqtt.git
cd pong-mqtt
cargo run --release
```

### 🎯 How to Play

1. **Run the game:**
   ```bash
   cargo run --release
   ```

2. **Select "Play Online (MQTT)" from the menu**

3. **Enter a Game ID** (e.g., "mygame")
   - Share this ID with your friend
   - Both players must use the **same Game ID** to join the same match

4. **Choose Player 1 or Player 2**
   - One person picks P1, the other picks P2
   - The game starts automatically when both players join

5. **Play!**
   - First to 5 points wins
   - Both players press **Space** after the game ends to ready up for a rematch

### 🎮 Controls

**In-Game:**
- `↑` / `W`: Move paddle up
- `↓` / `S`: Move paddle down
- Mouse scroll: Move paddle
- `Esc`: Quit to menu

**Game Over:**
- `Space`: Ready up for next game
- `Esc`: Return to main menu

---

## 🏗️ Architecture

### Server (Node.js/TypeScript)
- Hosted on AWS EC2 at `3.141.116.27:1883`
- Runs authoritative physics at 60fps
- Validates all game state and handles collisions
- Publishes ball position, scores, and game status via MQTT

### Client (Rust)
- Connects to MQTT broker
- Sends paddle position on movement
- Receives ball updates and renders at 60fps
- No local physics (server is source of truth)

### MQTT Topics
```
pong/game/{game_id}/p1/paddle   — P1 paddle position
pong/game/{game_id}/p2/paddle   — P2 paddle position
pong/game/{game_id}/ball        — Ball position/velocity (server → clients)
pong/game/{game_id}/state       — Scores and game status (server → clients)
pong/game/{game_id}/join        — Player join notifications
pong/game/{game_id}/ready       — Ready-up signals (post-game restart)
```

---

## 🛠️ Tech Stack

**Client:**
- Language: Rust
- UI: `ratatui` + `crossterm`
- MQTT: `rumqttc`

**Server:**
- Language: TypeScript (Node.js)
- MQTT: `mqtt` npm package
- Physics: Pure functional game logic at 60fps

---

## 🚀 Running Your Own Server

The game connects to a public MQTT broker by default. To host your own:

1. **Set up Mosquitto MQTT broker**
2. **Clone and deploy the server:**
   ```bash
   cd pong-server
   npm install
   npm run build
   npm start
   ```
3. **Update the client** to point to your broker in `src/network.rs`:
   ```rust
   broker_host: "your-server-ip".to_string(),
   ```

---

## 🎨 Themes

Choose from 7 color themes in the settings menu:
- Monokai
- Solarized
- Dracula
- Gruvbox Dark
- Nord
- One Dark
- High Contrast

---

## 🐛 Known Issues

- Terminal must be at least 60×20 for proper rendering
- Some terminal fonts don't support all Unicode block characters (use a modern terminal font)

---

## 📜 License

[MIT](LICENSE)

---

## 🙏 Credits

- **Original terminal.pong:** [IshmamR/terminal.pong](https://github.com/IshmamR/terminal.pong)
- **MQTT multiplayer extension:** Built for real-time technical discussion and demonstration

---

<div align="center">
Made with Rust 🦀 and MQTT 📡
</div>
