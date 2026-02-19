use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, MouseEvent, MouseEventKind};
use rand::random;
use std::{
    io,
    time::{Duration, Instant},
};

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::Style,
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
    Frame,
};

use crate::{
    game_theme::GameTheme,
    helpers::{centered_rect, string_to_char_array},
};

pub const PLAYER_NAME_CHAR_LEN: usize = 16;
const DEFAULT_BAR_LENGTH: u8 = 5;
const DEFAULT_BALL_VELOCITY_X: i8 = 3;
const DEFAULT_BALL_VELOCITY_Y: i8 = 1;
const DEFAULT_PADDLE_WIDTH: u16 = 3;
const STARTING_POWER_MOVES: u8 = 10;
const DEFAULT_DIFFICULTY: f32 = 1.0;

/// Physics space dimensions matching the server's GAME_CONSTANTS.
/// All network ball/paddle coordinates are in this space.
pub const COURT_WIDTH: f32 = 40.0;
pub const COURT_HEIGHT: f32 = 12.0;
pub const COURT_PADDLE_HEIGHT: f32 = 3.0;

#[derive(Debug, Clone, Copy)]
struct ComputerAI {
    reaction_delay: f32,     // Time before reacting to ball direction change
    last_ball_direction: i8, // Track ball direction changes
    reaction_timer: f32,     // Current reaction delay timer
    prediction_error: f32,   // How far off the prediction can be
    max_speed: f32,          // Maximum movement speed
    current_speed: f32,      // Current movement speed (with acceleration)
    target_position: f32,    // Where the AI wants to move
    // difficulty: f32,         // 0.0 to 1.0, affects all parameters
    fatigue: f32,         // Increases over time, affects performance
    last_update: Instant, // For delta time calculations
}

#[derive(Debug, Default)]
pub struct Player {
    pub name: [char; PLAYER_NAME_CHAR_LEN],
    pub score: u32,

    pub power_moves_left: u8,
    pub last_power_used_at: Option<Instant>,

    pub bar_position: u16,
    pub bar_length: u8,

    pub is_computer: bool,
    computer_ai: Option<ComputerAI>,
}

#[derive(Debug, Default)]
struct Ball {
    position: [u16; 2],
    velocity: [i8; 2],
    is_powered: bool,
    /// Server-authoritative position in physics/court units (WithNetwork mode only)
    net_x: f32,
    net_y: f32,
    /// Server-reported velocity (court units / server tick, WithNetwork mode only)
    net_dx: f32,
    net_dy: f32,
    /// Client-side smoothed display position (dead reckoning, WithNetwork mode only)
    render_x: f32,
    render_y: f32,
}

#[derive(Debug, PartialEq)]
pub enum GameType {
    ScreenSaver,
    WithNetwork,
}


#[derive(Debug)]
pub struct Game {
    game_type: GameType,
    players: [Player; 2],
    ball: Ball,
    game_area: Rect,
    last_update: Instant,
    is_paused: bool,
    scored_keep_display: bool,
    difficulty: f32,
    should_exit: bool,
    theme: GameTheme,
    /// For network games: which player index (0 or 1) this client controls.
    /// None means local (both paddles controlled locally).
    local_player_index: Option<usize>,
    /// Set to true when the player presses Space to serve; cleared by App after publishing.
    pub pending_serve: bool,
    /// True once the first BallUpdate from the server has been received.
    /// Prevents rendering the ball at its dummy initial position before server data arrives.
    has_ball_data: bool,
}

impl Game {
    pub fn set_theme(&mut self, theme: GameTheme) {
        self.theme = theme;
    }

    /// Set which player index (0-based) this client controls in a network game
    pub fn set_local_player_index(&mut self, index: usize) {
        self.local_player_index = Some(index);
    }

    /// Drive the opponent's paddle from a network event.
    /// y_physics is in court units (0 .. COURT_HEIGHT - COURT_PADDLE_HEIGHT = 9).
    pub fn set_opponent_paddle(&mut self, player_index: usize, y_physics: f32) {
        let inner_height = self.game_area.height.saturating_sub(2) as f32;
        if inner_height <= 0.0 { return; }
        let bar_len = self.players[player_index].bar_length as f32;
        let max_y = (inner_height - bar_len).max(0.0);
        // Physics y range: 0 .. (COURT_HEIGHT - COURT_PADDLE_HEIGHT)
        // Terminal y range: 0 .. max_y
        let phys_range = (COURT_HEIGHT - COURT_PADDLE_HEIGHT).max(1.0);
        let terminal_y = (y_physics / phys_range * max_y).clamp(0.0, max_y);
        self.players[player_index].bar_position = terminal_y as u16;
    }

    /// Store the authoritative ball state from the server (physics/court units).
    /// Always snaps render_x/y to server truth — the server IS the physics engine.
    /// No lerp: lerp caused the rendered ball to lag perpetually behind server position.
    pub fn set_ball_from_network(&mut self, x: f32, y: f32, dx: f32, dy: f32) {
        self.ball.net_x = x;
        self.ball.net_y = y;
        self.ball.net_dx = dx;
        self.ball.net_dy = dy;
        self.ball.render_x = x;
        self.ball.render_y = y;
        self.has_ball_data = true;
    }

    /// Overwrite scores from the server state message
    pub fn set_scores(&mut self, p1: u32, p2: u32) {
        self.players[0].score = p1;
        self.players[1].score = p2;
    }

    /// Return the local paddle Y in physics/court units (for publishing to server).
    pub fn get_paddle_physics_y(&self, player_index: usize) -> f32 {
        let inner_height = self.game_area.height.saturating_sub(2) as f32;
        let bar_len = self.players[player_index].bar_length as f32;
        let max_terminal = (inner_height - bar_len).max(1.0);
        let terminal_y = self.players[player_index].bar_position as f32;
        // Map terminal position → physics position (0 .. COURT_HEIGHT - COURT_PADDLE_HEIGHT)
        (terminal_y / max_terminal) * (COURT_HEIGHT - COURT_PADDLE_HEIGHT)
    }
}

impl Game {
    pub fn new(
        player_names: [&str; 2],
        game_area: Rect,
        game_type: GameType,
        difficulty: Option<f32>,
    ) -> Self {
        let theme = GameTheme::Monokai;

        let final_difficulty = difficulty.unwrap_or(DEFAULT_DIFFICULTY).clamp(0.0, 2.0);
        let ai_player = ComputerAI {
            reaction_delay: 0.2 + (2.0 - final_difficulty) * 0.5, // 0.2-0.7 seconds
            last_ball_direction: 0,
            reaction_timer: 0.0,
            prediction_error: 2.0 + (1.0 - final_difficulty) * 2.5, // 2-7 units error
            max_speed: 0.8 + final_difficulty * 0.85,               // 0.8-2.5 speed
            current_speed: 0.0,
            target_position: 0.0,
            fatigue: 0.0,
            last_update: Instant::now(),
        };

        let player1 = Player {
            name: string_to_char_array(player_names[0]),
            bar_position: (game_area.height / 2).saturating_sub((DEFAULT_BAR_LENGTH / 2) as u16),
            bar_length: DEFAULT_BAR_LENGTH,
            is_computer: false,
            computer_ai: if game_type == GameType::ScreenSaver {
                Some(ai_player.clone())
            } else {
                None
            },
            power_moves_left: STARTING_POWER_MOVES,
            last_power_used_at: None,
            score: 0,
        };

        let player2 = Player {
            name: string_to_char_array(player_names[1]),
            bar_position: (game_area.height / 2).saturating_sub((DEFAULT_BAR_LENGTH / 2) as u16),
            bar_length: DEFAULT_BAR_LENGTH,
            is_computer: false,
            computer_ai: if game_type == GameType::ScreenSaver {
                Some(ai_player.clone())
            } else {
                None
            },
            power_moves_left: STARTING_POWER_MOVES,
            last_power_used_at: None,
            score: 0,
        };

        Self {
            game_type,
            players: [player1, player2],
            ball: Ball {
                position: [
                    game_area.width.saturating_sub(4) / 2,
                    game_area.height.saturating_sub(4) / 2,
                ],
                velocity: [DEFAULT_BALL_VELOCITY_X, DEFAULT_BALL_VELOCITY_Y],
                is_powered: false,
                net_x: COURT_WIDTH / 2.0,
                net_y: COURT_HEIGHT / 2.0,
                net_dx: 0.0,
                net_dy: 0.0,
                render_x: COURT_WIDTH / 2.0,
                render_y: COURT_HEIGHT / 2.0,
            },
            last_update: Instant::now(),
            game_area: game_area,
            is_paused: false,
            scored_keep_display: false,
            difficulty: final_difficulty,
            should_exit: false,
            theme,
            local_player_index: None,
            pending_serve: false,
            has_ball_data: false,
        }
    }

    pub fn get_area(&self) -> Rect {
        self.game_area
    }

    pub fn set_area(&mut self, game_area: Rect) {
        self.game_area = game_area;
    }

    pub fn get_player(&self, index: usize) -> &Player {
        &self.players[index]
    }

    fn move_player(&mut self, player_index: usize, direction: i8) {
        if direction == 0 {
            return;
        }

        let player = &mut self.players[player_index];

        if player.is_computer {
            return;
        }

        let step: u16 = 1;

        if direction > 0 {
            // up
            if player.bar_position > 0 {
                player.bar_position -= step;
            }
        } else {
            // down
            let inner_height = self.game_area.height.saturating_sub(2);
            if player.bar_position + (player.bar_length as u16) < inner_height {
                player.bar_position += step;
            }
        }
    }

    fn handle_key_event(&mut self, key_event: KeyEvent) {
        let code = key_event.code;
        match code {
            KeyCode::Esc | KeyCode::Char('q') => self.should_exit = true,
            KeyCode::Char('p') => {
                if self.game_type != GameType::WithNetwork {
                    self.toggle_pause();
                }
            }
            _ => {
                if let Some(local_idx) = self.local_player_index {
                    // Network mode: Up/W move local paddle up, Down/S move it down.
                    // Both key sets work so either hand feels natural.
                    match code {
                        KeyCode::Up | KeyCode::Char('w') => self.move_player(local_idx, 1),
                        KeyCode::Down | KeyCode::Char('s') => self.move_player(local_idx, -1),
                        KeyCode::Char(' ') | KeyCode::Enter => self.pending_serve = true,
                        _ => {}
                    }
                } else {
                    // Local / screensaver mode: original two-player bindings
                    match code {
                        KeyCode::Char('/') => self.power_move(0),
                        KeyCode::Up => self.move_player(0, 1),
                        KeyCode::Down => self.move_player(0, -1),
                        KeyCode::Char(' ') => self.power_move(1),
                        KeyCode::Char('w') => self.move_player(1, 1),
                        KeyCode::Char('s') => self.move_player(1, -1),
                        _ => {}
                    }
                }
            }
        }
    }

    fn handle_mouse_event(&mut self, mouse_event: MouseEvent) {
        // Always scroll the local player's paddle.
        // Previously hardcoded to 0, which meant player-2 clients were accidentally
        // moving the opponent's (index 0) rendered paddle on their screen.
        let idx = self.local_player_index.unwrap_or(0);
        match mouse_event.kind {
            MouseEventKind::ScrollUp => self.move_player(idx, 1),
            MouseEventKind::ScrollDown => self.move_player(idx, -1),
            _ => {}
        }
    }

    fn handle_events(&mut self) -> io::Result<()> {
        // Process all pending events for better responsiveness
        while event::poll(Duration::from_millis(5))? {
            match event::read()? {
                Event::Mouse(mouse_event) => self.handle_mouse_event(mouse_event),
                Event::Key(key_event) if key_event.kind == KeyEventKind::Press => {
                    self.handle_key_event(key_event)
                }
                _ => {}
            }
        }
        Ok(())
    }

    // key events while paused (pause/options popup)
    fn handle_pause_events(&mut self) -> io::Result<()> {
        while event::poll(Duration::from_millis(5))? {
            match event::read()? {
                Event::Key(key_event) if key_event.kind == KeyEventKind::Press => {
                    match key_event.code {
                        KeyCode::Char('p') => self.is_paused = false, // Resume
                        KeyCode::Enter => self.is_paused = false,     // Resume
                        KeyCode::Esc => self.should_exit = true,
                        KeyCode::Char('d') => {
                            // Cycle through all available themes
                            self.theme = match self.theme {
                                GameTheme::Monokai => GameTheme::Solarized,
                                GameTheme::Solarized => GameTheme::Dracula,
                                GameTheme::Dracula => GameTheme::GruvboxDark,
                                GameTheme::GruvboxDark => GameTheme::Nord,
                                GameTheme::Nord => GameTheme::OneDark,
                                GameTheme::OneDark => GameTheme::HighContrast,
                                GameTheme::HighContrast => GameTheme::Monokai,
                            };
                        }
                        KeyCode::Left => {
                            self.difficulty = (self.difficulty - 0.1).clamp(0.0, 2.0);
                        }
                        KeyCode::Right => {
                            self.difficulty = (self.difficulty + 0.1).clamp(0.0, 2.0);
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn toggle_pause(&mut self) {
        self.is_paused = !self.is_paused;
    }

    /**
     * 1 -> ball collision with Player 1's bar
     * 2 -> ball collision with Player 2's bar
     * None -> no collision, ball position updated normally
     */
    fn update_ball_position(&mut self) -> Option<u8> {
        let inner_width = self.game_area.width.saturating_sub(3);
        let inner_height = self.game_area.height.saturating_sub(2);

        let players = &self.players;
        let ball = &mut self.ball;

        let new_x = ball.position[0].saturating_add_signed(ball.velocity[0] as i16);
        let new_y = ball.position[1].saturating_add_signed(ball.velocity[1] as i16);

        // collision with top and bottom walls
        if new_y == 0 || new_y >= inner_height {
            ball.velocity[1] = -ball.velocity[1];
            ball.position[1] = if new_y == 0 { 0 } else { inner_height - 1 };
        } else {
            ball.position[1] = new_y;
        }

        if !self.scored_keep_display {
            // ball collision with Player 1's bar (left side)
            if new_x <= DEFAULT_PADDLE_WIDTH && ball.velocity[0] < 0 {
                if new_y >= players[0].bar_position
                    && new_y < players[0].bar_position + players[0].bar_length as u16
                {
                    ball.velocity[0] = -ball.velocity[0];
                    ball.position[0] = DEFAULT_PADDLE_WIDTH;
                    return Some(1);
                }
            }

            // ball collision with Player 2's bar (right side)
            if new_x >= inner_width - DEFAULT_PADDLE_WIDTH - 1 && ball.velocity[0] > 0 {
                if new_y >= players[1].bar_position
                    && new_y < players[1].bar_position + players[1].bar_length as u16
                {
                    ball.velocity[0] = -DEFAULT_BALL_VELOCITY_X;
                    ball.position[0] = inner_width - DEFAULT_PADDLE_WIDTH - 1;
                    ball.is_powered = false;
                    return Some(2);
                }
            }
        }

        // ball went off screen (reset)
        if new_x < DEFAULT_PADDLE_WIDTH || new_x > inner_width - DEFAULT_PADDLE_WIDTH - 1 {
            if new_x <= 0 || new_x >= inner_width {
                // Ball exited the screen: left or right
                if new_x <= 0 {
                    // ball exited on the left → player missed → computer scores
                    self.players[1].score += 1;
                } else {
                    // ball exited on the right → computer missed → player scores
                    self.players[0].score += 1;
                }

                // reset ball to center
                ball.position = [
                    inner_width / 2,
                    rand::random_range(1..inner_height.saturating_sub(1)),
                ];

                let random_number: i16 = rand::random_range(0..=1);
                let direction = if random_number == 0 { 1 } else { -1 };

                ball.velocity[0] = direction * DEFAULT_BALL_VELOCITY_X;
                ball.is_powered = false;

                self.scored_keep_display = false;

                return None;
            } else {
                // keep drawing
                self.scored_keep_display = true;
                ball.position[0] = new_x;
            }
        } else {
            ball.position[0] = new_x;
        }

        None
    }

    fn update_computer_player(&mut self, player_index: usize) {
        let computer = &mut self.players[player_index];
        let ball = &self.ball;

        if computer.computer_ai.is_none() {
            return;
        }
        let ai = computer.computer_ai.as_mut().unwrap();

        let inner_height = self.game_area.height;
        let paddle_x = if player_index == 0 {
            DEFAULT_PADDLE_WIDTH // Player 1's paddle is on the left
        } else {
            self.game_area.width - DEFAULT_PADDLE_WIDTH // Player 2's paddle is on the right
        };

        // accumulate delta time to add fatigue
        let dt = ai.last_update.elapsed().as_secs_f32();
        ai.last_update = Instant::now();
        // increase fatigue over time
        if self.game_type == GameType::ScreenSaver {
            ai.fatigue = (ai.fatigue + dt * 0.001).min(0.05); // much less fatigue for AI vs AI
        } else {
            ai.fatigue = (ai.fatigue + dt * 0.009).min(0.3);
        }

        // now, let's calculate the direction the ball is moving towards
        let ball_direction_x: i8 = ball.velocity[0].signum(); // one of these 3 -> { -1, 0, 1 }

        // if ball direction changed, add a reaction timer
        if ball_direction_x != ai.last_ball_direction && ball_direction_x != 0 {
            ai.last_ball_direction = ball_direction_x;
            ai.reaction_timer = ai.reaction_delay + ai.fatigue * 0.5;
        }

        ai.reaction_timer = (ai.reaction_timer - dt).max(0.0);

        // check if ball is coming towards computer paddle
        let is_ball_coming = if player_index == 0 {
            ball.velocity[0] < 0 // Player 1's paddle is on the left
        } else {
            ball.velocity[0] > 0 // Player 2's paddle is on the right
        };

        let paddle_center = computer.bar_position as f32 + computer.bar_length as f32 / 2.0;

        if !is_ball_coming || ai.reaction_timer > 0.0 {
            // neutral positioning
            // slowly drift towards the center

            let center_y = inner_height / 2;
            ai.target_position = paddle_center + (center_y as f32 - paddle_center) * 0.1;
        } else {
            // active/predictive positioning
            // "predict" ball position with wall bounces

            let time_to_paddle_x =
                (paddle_x as f32 - ball.position[0] as f32) / ball.velocity[0] as f32;
            let mut pred_y = ball.position[1] as f32 + ball.velocity[1] as f32 * time_to_paddle_x;

            // simulate top and bottom wall bounces
            while pred_y < 0.0 || pred_y > inner_height as f32 {
                if pred_y < 0.0 {
                    pred_y = -pred_y; // bounce off top wall
                } else {
                    pred_y = 2.0 * inner_height as f32 - pred_y; // bounce off bottom wall
                }
            }

            // sprinkle some prediction errors -,-
            let (error_magnitude, oops_chance, random_chance) = match self.game_type {
                GameType::ScreenSaver => (ai.prediction_error * 0.3, 0.01, 0.02), // much less error
                _ => (
                    ai.prediction_error * (1.0 + ai.fatigue),
                    0.05 + ai.fatigue * 0.1,
                    0.1,
                ),
            };
            let prediction_error = (random::<f32>() - 0.5) * error_magnitude;
            pred_y += prediction_error;

            // make big oopsies occasionally
            if random::<f32>() < oops_chance {
                pred_y += (random::<f32>() - 0.5) * 3.0;
            }

            // add some final randomness
            if random::<f32>() < random_chance {
                pred_y += (random::<f32>() - 0.5) * 1.0;
            }

            // clamp to fix
            pred_y = pred_y.clamp(0.0, (inner_height - computer.bar_length as u16) as f32);

            ai.target_position = pred_y;
        }

        // smooth movement with acceleration
        let distance_to_target = ai.target_position - paddle_center;
        let desired_speed = distance_to_target.abs().min(ai.max_speed);
        let acceleration = 2.0;
        if distance_to_target.abs() > 0.5 {
            ai.current_speed = (ai.current_speed + acceleration * dt).min(desired_speed);
        } else {
            ai.current_speed = (ai.current_speed - acceleration * dt * 2.0).max(0.0);
        }

        // add some jitter and behavioral quirks
        let jitter = match self.game_type {
            GameType::ScreenSaver => (random::<f32>() - 0.5) * 0.02 * (1.0 + ai.fatigue),
            _ => 0.0,
        };
        let movement = distance_to_target.signum() * ai.current_speed + jitter;

        let final_movement = match self.game_type {
            GameType::ScreenSaver => movement,
            _ => movement,
        };

        // apply new position with clamping
        let new_pos = (paddle_center + final_movement).clamp(
            computer.bar_length as f32 / 2.0,
            (inner_height - computer.bar_length as u16) as f32,
        );

        computer.bar_position = (new_pos - computer.bar_length as f32 / 2.0) as u16;
    }

    fn power_move(&mut self, player_index: usize) {
        let player = &mut self.players[player_index];

        if player.power_moves_left <= 0 {
            return; // no power move left
        }

        let ball = &mut self.ball;

        let is_ball_approaching = if player_index == 0 {
            ball.velocity[0] < 0
        } else {
            ball.velocity[0] > 0
        };

        let within_bar = ball.position[1] >= player.bar_position
            && ball.position[1] < player.bar_position + player.bar_length as u16;

        let min_range = 4.0;
        let max_range = 12.0;
        let allowed_range = (max_range - min_range) * (1.0 - self.difficulty) + min_range;
        let allowed_range = allowed_range.round() as u16;

        let within_x = if player_index == 0 {
            ball.position[0] > 1 && ball.position[0] < 1 + allowed_range
        } else {
            let right_edge = self.game_area.width.saturating_sub(1);
            ball.position[0] > right_edge.saturating_sub(allowed_range)
                && ball.position[0] < right_edge.saturating_sub(1)
        };

        if is_ball_approaching && within_bar && within_x {
            // power move: send ball flying in the correct direction
            ball.velocity[0] = if player_index == 0 { 6 } else { -6 };
            ball.is_powered = true;
            player.power_moves_left -= 1;
            player.last_power_used_at = Some(Instant::now());
        }
    }

    fn draw_core_elements(&self, frame: &mut Frame) {
        let colors = self.theme.colors();
        let game_area = self.get_area();
        let inner_area = Rect::new(
            game_area.x + 1,
            game_area.y + 1,
            game_area.width.saturating_sub(2),
            game_area.height.saturating_sub(2),
        );

        // Clear the entire play field every frame.
        // Ratatui alternates two buffers and never resets them between draws, so
        // any cell not explicitly written this frame retains its content from
        // *two frames ago*.  Without this Clear, old ball/paddle positions bleed
        // back into the terminal diff and appear as ghost objects.
        frame.render_widget(Clear, inner_area);

        // Player 1 bar (left side)
        let player1 = self.get_player(0);
        let bar_1_area = Rect::new(
            inner_area.x,
            inner_area.y + player1.bar_position,
            3,
            player1.bar_length as u16,
        );
        let bar_1_color = if let Some(last) = player1.last_power_used_at {
            if last.elapsed() < Duration::from_millis(200) {
                colors.player_bar_power
            } else {
                colors.player_bar
            }
        } else {
            colors.player_bar
        };
        let bar_1 = Block::default()
            .borders(Borders::ALL)
            .style(Style::default().fg(colors.player_bar).bg(bar_1_color));
        frame.render_widget(bar_1, bar_1_area);

        // Player 2 bar (right side)
        let player2 = self.get_player(1);
        let bar_2_area = Rect::new(
            inner_area.x + inner_area.width - 4,
            inner_area.y + player2.bar_position,
            3,
            player2.bar_length as u16,
        );
        let bar_2_color = if let Some(last) = player2.last_power_used_at {
            if last.elapsed() < Duration::from_millis(200) {
                colors.player_bar_power
            } else {
                colors.player_bar
            }
        } else {
            colors.player_bar
        };
        let bar_2 = Block::default()
            .borders(Borders::ALL)
            .style(Style::default().fg(colors.player_bar).bg(bar_2_color));
        frame.render_widget(bar_2, bar_2_area);

        // Ball — always render. The Clear widget above wipes stale buffer content
        // each frame, so the phantom-before-server-data problem no longer exists.
        {
            let (ball_col, ball_row) = if self.game_type == GameType::WithNetwork {
                let iw = inner_area.width.saturating_sub(2) as f32;
                // Server wall-bounce clamps ball Y to [0, COURT_HEIGHT-1].
                // Map that range to the full inner height (minus 1 for ball height=1).
                let ih = inner_area.height.saturating_sub(1) as f32;
                let col = ((self.ball.render_x / COURT_WIDTH) * iw).clamp(0.0, iw) as u16;
                let row = ((self.ball.render_y / (COURT_HEIGHT - 1.0)) * ih).clamp(0.0, ih) as u16;
                (col, row)
            } else {
                (self.ball.position[0], self.ball.position[1])
            };
            let ball_area = Rect::new(
                inner_area.x + ball_col,
                inner_area.y + ball_row,
                2,
                1,
            );
            let ball = Paragraph::new("██").style(Style::default().fg(colors.ball));
            frame.render_widget(ball, ball_area);
        }
    }

    pub fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();
        let colors = self.theme.colors();

        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints(vec![
                Constraint::Fill(1),   // game block - fills available space
                Constraint::Length(3), // controls block
            ])
            .split(area);

        let game_area = layout[0];
        self.set_area(game_area);

        let title = self.get_block_title("terminal.pong");
        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_type(BorderType::Thick)
            .style(Style::default().fg(colors.border).bg(colors.background))
            .title_alignment(Alignment::Center);
        frame.render_widget(block, game_area);

        self.draw_core_elements(frame);

        let controls_text = match self.local_player_index {
            Some(0) => " P1: ↑/W = up  ↓/S = down  Space = serve  |  Esc = Quit ",
            Some(1) => " P2: ↑/W = up  ↓/S = down  Space = serve  |  Esc = Quit ",
            _ => " Player 1: ↑/↓  |  Player 2: W/S  |  P=Pause  |  Esc=Quit ",
        };
        let controls = Paragraph::new(controls_text)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .style(Style::default().fg(colors.border)),
            )
            .style(Style::default().fg(colors.text))
            .alignment(Alignment::Center);
        frame.render_widget(controls, layout[1]);

        if self.is_paused {
            // draw pause/options popup if paused
            let popup_width = 52;
            let popup_height = 12;
            let popup_area = centered_rect(popup_width, popup_height, area.width, area.height);
            let popup_block = Block::default()
                .title("Paused - Options")
                .borders(Borders::ALL)
                .border_type(BorderType::Double)
                .style(Style::default().fg(colors.accent))
                .title_alignment(Alignment::Center);
            frame.render_widget(popup_block, popup_area);

            // options and instructions
            let diff_label = match self.difficulty {
                d if d < 0.6 => "Easy",
                d if d < 1.3 => "Normal",
                _ => "Hard",
            };
            let theme_label = match self.theme {
                GameTheme::Monokai => "Monokai",
                GameTheme::Solarized => "Solarized",
                GameTheme::Dracula => "Dracula",
                GameTheme::GruvboxDark => "Gruvbox Dark",
                GameTheme::Nord => "Nord",
                GameTheme::OneDark => "One Dark",
                GameTheme::HighContrast => "High Contrast",
            };
            let options_text = format!(
                "\n  Difficulty: {} ({:.2})\n [←/→] Adjust  [D] Toggle Theme (Current: {})\n  [P/Enter] Resume  [Esc] Quit\n",
                diff_label, self.difficulty, theme_label
            );
            let options = Paragraph::new(options_text)
                .style(Style::default().fg(colors.text))
                .alignment(Alignment::Center);
            let options_area = Rect::new(
                popup_area.x + 2,
                popup_area.y + 2,
                popup_area.width - 4,
                popup_area.height - 4,
            );
            frame.render_widget(options, options_area);
        }
    }

    pub fn game_loop(&mut self) -> io::Result<bool> {
        // If paused, only handle pause menu events
        if self.is_paused {
            self.handle_pause_events()?;
            if self.should_exit {
                return Ok(false);
            }
            return Ok(true);
        }

        if self.game_type == GameType::WithNetwork {
            // Network mode: server owns all ball physics.
            // Process input every frame — no timer gate. The 40fps timer was built
            // for local ball physics speed control and has no purpose here; it only
            // added up to 25ms of input lag on the local paddle.
            self.handle_events()?;
            if self.should_exit {
                return Ok(false);
            }
        } else {
            // ScreenSaver: local physics at difficulty-scaled FPS (15–40 Hz).
            let fps = 15.0_f32 + 25.0 * self.difficulty;
            let each_frame = (1000.0 / fps).round() as u64;

            if self.last_update.elapsed() >= Duration::from_millis(each_frame) {
                self.handle_events()?;
                if self.should_exit {
                    return Ok(false);
                }
                if rand::random() {
                    self.update_computer_player(0);
                    self.update_computer_player(1);
                } else {
                    self.update_computer_player(1);
                    self.update_computer_player(0);
                }
                let _ = self.update_ball_position();
                self.last_update = Instant::now();
            }
        }

        Ok(true)
    }

    fn get_block_title(&self, app_name: &'static str) -> String {
        let player1 = self.get_player(0);
        let p1_text = format!(
            "{} ({})",
            player1.name.iter().collect::<String>().trim_end().to_string(),
            player1.score
        );

        let player2 = self.get_player(1);
        let p2_text = format!(
            "({}) {}",
            player2.score,
            player2.name.iter().collect::<String>().trim_start().to_string()
        );

        let used = p1_text.len() + app_name.len() + p2_text.len() + 6; // spaces + separators
        let total_width = self.game_area.width as usize;
        let dashes = total_width.saturating_sub(used) / 2;

        format!(
            " {} {} {} {} {} ",
            p1_text,
            "─".repeat(dashes),
            app_name,
            "─".repeat(dashes),
            p2_text,
        )
    }
}
