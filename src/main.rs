use std::{
    io::{self},
    sync::mpsc,
    thread::sleep,
    time::{Duration, Instant},
};

use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    ExecutableCommand,
};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Flex, Layout, Margin, Rect},
    style::{Color, Style, Stylize},
    widgets::{Block, BorderType, Borders, Paragraph},
    DefaultTerminal, Frame,
};
use tui_big_text::{BigText, PixelSize};

mod game;
mod game_theme;
mod helpers;
mod network;
use crate::{
    game::{Game, GameType},
    helpers::centered_rect_with_percentage,
    network::{NetworkConfig, NetworkEvent},
};

#[derive(Debug)]
struct MainMenu {
    options: Vec<&'static str>,
    selected: usize,
}

#[derive(Debug)]
enum AppScreen {
    MainMenu,
    NetworkLobby,
    Game,
}

use crate::game_theme::GameTheme;

struct App {
    exit: bool,
    main_menu: MainMenu,
    current_game: Option<Game>,
    screen: AppScreen,
    selected_theme: GameTheme,
    // Network lobby state
    network_rx: Option<mpsc::Receiver<NetworkEvent>>,
    network_paddle_tx: Option<mpsc::SyncSender<f32>>,
    network_local_player: u8,     // 1 or 2
    network_game_id: String,      // typed game ID
    network_player_select: u8,    // lobby: which player slot selected (1 or 2)
    network_lobby_field: usize,   // 0=game_id, 1=player, 2=connect, 3=back
    network_last_paddle_y: f32,   // debounce: only publish when changed (physics units)
    network_status: NetworkStatus,
    network_serve_tx: Option<mpsc::SyncSender<()>>,
    network_restart_tx: Option<mpsc::SyncSender<()>>,
    network_ready_tx: Option<mpsc::SyncSender<()>>,
    game_over: bool,  // Track when game ends for overlay UI
}

#[derive(Debug, PartialEq)]
enum NetworkStatus {
    Idle,
    Connecting,
    Connected,
    Disconnected,
}

const MAIN_MENU_OPTIONS: [&str; 2] = [
    "Play Online (MQTT)",
    "Exit",
];
const MENU_LAST_IDX: usize = MAIN_MENU_OPTIONS.len() - 1;

impl App {
    fn new() -> Self {
        let main_menu = MainMenu {
            options: MAIN_MENU_OPTIONS.to_vec(),
            selected: 0,
        };

        Self {
            exit: false,
            main_menu: main_menu,
            current_game: None,
            screen: AppScreen::MainMenu,
            selected_theme: GameTheme::Monokai,
            network_rx: None,
            network_paddle_tx: None,
            network_local_player: 1,
            network_game_id: String::from("demo"),
            network_player_select: 1,
            network_lobby_field: 0,
            network_last_paddle_y: -1.0,
            network_status: NetworkStatus::Idle,
            network_serve_tx: None,
            network_restart_tx: None,
            network_ready_tx: None,
            game_over: false,
        }
    }

    pub fn run(&mut self, mut terminal: DefaultTerminal) -> io::Result<()> {
        let mut last_size: u8 = 0; // 0 -> too small | 1 -> normal

        while !self.exit {
            let min_width = 60;
            let min_height = 20;

            let size = terminal.size()?;
            if size.width < min_width || size.height < min_height {
                if last_size == 1 {
                    sleep(Duration::from_millis(100));
                    last_size = 0;
                }
                self.handle_events()?;
                terminal.draw(|frame| self.show_terminal_resize_warning(frame))?;
            } else {
                if last_size == 0 {
                    sleep(Duration::from_millis(100));
                    last_size = 1;
                }

                match self.screen {
                    AppScreen::MainMenu => {
                        self.handle_events()?;
                        let _ = terminal.draw(|frame| self.draw(frame));
                    }
                    AppScreen::NetworkLobby => {
                        self.handle_network_lobby_events()?;
                        let _ = terminal.draw(|frame| self.draw_network_lobby(frame));
                    }
                    AppScreen::Game => {
                        let frame_start = Instant::now();

                        // Drain MQTT events before the game loop tick
                        self.drain_network_events();

                        // Handle game over input (Space to ready up)
                        if self.game_over {
                            if event::poll(Duration::from_millis(5))? {
                                if let Event::Key(key_event) = event::read()? {
                                    if key_event.kind == KeyEventKind::Press {
                                        match key_event.code {
                                            KeyCode::Char(' ') | KeyCode::Enter => {
                                                if let Some(tx) = &self.network_ready_tx {
                                                    tx.try_send(()).ok();
                                                }
                                            }
                                            KeyCode::Esc => {
                                                self.current_game = None;
                                                self.network_rx = None;
                                                self.network_paddle_tx = None;
                                                self.network_serve_tx = None;
                                                self.network_restart_tx = None;
                                                self.network_ready_tx = None;
                                                self.game_over = false;
                                                self.network_status = NetworkStatus::Idle;
                                                self.screen = AppScreen::MainMenu;
                                                continue;
                                            }
                                            _ => {}
                                        }
                                    }
                                }
                            }
                        }

                        let continue_game = if !self.game_over {
                            match self.current_game.as_mut() {
                                Some(game) => game.game_loop()?,
                                None => false,
                            }
                        } else {
                            true  // Don't exit when game over, just show overlay
                        };

                        if !continue_game {
                            self.current_game = None;
                            self.network_rx = None;
                            self.network_paddle_tx = None;
                            self.network_serve_tx = None;
                            self.network_restart_tx = None;
                            self.network_ready_tx = None;
                            self.game_over = false;
                            self.network_status = NetworkStatus::Idle;
                            self.screen = AppScreen::MainMenu;
                        } else {
                            // Check if player wants to serve
                            let wants_serve = self.current_game.as_ref().map(|g| g.pending_serve).unwrap_or(false);
                            if wants_serve {
                                if let Some(game) = self.current_game.as_mut() {
                                    game.pending_serve = false;
                                }
                                if let Some(tx) = &self.network_serve_tx {
                                    tx.try_send(()).ok();
                                }
                            }

                            // Publish our paddle Y (physics units) if it changed
                            let local_idx = self.network_local_player.saturating_sub(1) as usize;
                            let paddle_y = self.current_game.as_ref().map(|g| g.get_paddle_physics_y(local_idx));
                            if let Some(y) = paddle_y {
                                if (y - self.network_last_paddle_y).abs() > 0.01 {
                                    self.network_last_paddle_y = y;
                                    if let Some(tx) = &self.network_paddle_tx {
                                        tx.try_send(y).ok();
                                    }
                                }
                            }
                            if let Some(game) = self.current_game.as_mut() {
                                let game_over = self.game_over;
                                let _ = terminal.draw(|frame| {
                                    game.draw(frame);
                                    if game_over {
                                        Self::draw_game_over_overlay(frame, game);
                                    }
                                });
                            }

                            // Cap the render loop at ~60fps so the terminal isn't flooded
                            // with escape sequences and causes tearing / ghost artifacts.
                            const FRAME_TARGET: Duration = Duration::from_millis(16);
                            let elapsed = frame_start.elapsed();
                            if elapsed < FRAME_TARGET {
                                sleep(FRAME_TARGET - elapsed);
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }

    fn show_terminal_resize_warning(&mut self, frame: &mut Frame) {
        let colors = self.selected_theme.colors();
        let area = frame.area();
        let popup_area = centered_rect_with_percentage(60, 20, area.width, area.height);
        let popup = Paragraph::new("Terminal too small!\nPlease resize.")
            .block(
                Block::default()
                    .title("Warning")
                    .borders(Borders::ALL)
                    .border_type(BorderType::Thick),
            )
            .style(Style::default().fg(colors.ball))
            .alignment(Alignment::Center);
        frame.render_widget(popup, popup_area);
    }

    fn draw(&mut self, frame: &mut Frame) {
        let vertical_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints(vec![
                Constraint::Length(9),
                Constraint::Length(9),
                Constraint::Max(5),
            ])
            .flex(Flex::Center)
            .split(frame.area());

        let big_text = BigText::builder()
            .pixel_size(PixelSize::HalfHeight)
            .lines(vec![
                "PONG".white().into(),
                "MQTT".cyan().into(),
            ])
            .alignment(Alignment::Center)
            .build();
        frame.render_widget(big_text, vertical_layout[0]);

        let options_block_layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(vec![Constraint::Percentage(30)])
            .flex(Flex::Center)
            .split(vertical_layout[1]);
        frame.render_widget(
            Block::default()
                .title("")
                .style(Style::default().fg(Color::Cyan))
                .borders(Borders::ALL)
                .border_type(BorderType::Double),
            options_block_layout[0],
        );

        let options_layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(vec![Constraint::Percentage(90)])
            .flex(Flex::Center)
            .split(options_block_layout[0]);

        let inner_options_layout = options_layout[0].inner(Margin::new(1, 0));
        let rows_stored = inner_options_layout.height.clamp(7, 20) as usize;

        let option_constraints = vec![Constraint::Max(1); rows_stored];
        let option_areas = Layout::vertical(option_constraints)
            .flex(Flex::Center)
            .split(inner_options_layout);

        let empty_line = Paragraph::new("")
            .style(Style::default())
            .alignment(Alignment::Center);

        frame.render_widget(empty_line.clone(), option_areas[0]);
        for (i, &option) in self.main_menu.options.iter().enumerate() {
            let mut option_widget = Paragraph::new(option)
                .style(Style::default().fg(Color::Green).bold())
                .alignment(Alignment::Center);

            if i == self.main_menu.selected {
                option_widget = option_widget.style(
                    Style::default()
                        .bg(Color::Reset)
                        .fg(Color::White)
                        .bold()
                        .italic(),
                );
            }

            frame.render_widget(option_widget, option_areas[(i + 1) * 2]);
        }
        frame.render_widget(empty_line, option_areas[0]);
    }

    fn handle_events(&mut self) -> io::Result<()> {
        // Non-blocking event polling with short timeout
        if event::poll(Duration::from_millis(10))? {
            match event::read()? {
                Event::Key(key_event) if key_event.kind == KeyEventKind::Press => {
                    match key_event.code {
                        KeyCode::Char('q') => self.exit(),
                        KeyCode::Up => {
                            if self.main_menu.selected > 0 {
                                self.main_menu.selected -= 1;
                            } else {
                                self.main_menu.selected = MENU_LAST_IDX;
                            }
                        }
                        KeyCode::Down => {
                            if self.main_menu.selected < MENU_LAST_IDX {
                                self.main_menu.selected += 1;
                            } else {
                                self.main_menu.selected = 0;
                            }
                        }
                        KeyCode::Enter => {
                            match self.main_menu.selected {
                                0 => {
                                    // Play Online
                                    self.network_lobby_field = 0;
                                    self.network_game_id = String::from("demo");
                                    self.network_player_select = 1;
                                    self.screen = AppScreen::NetworkLobby;
                                }
                                1 => {
                                    self.exit();
                                }
                                _ => {}
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Network lobby
    // -----------------------------------------------------------------------

    fn draw_network_lobby(&mut self, frame: &mut Frame) {
        let area = frame.area();
        let popup_area = centered_rect_with_percentage(50, 40, area.width, area.height);

        let status_label = match self.network_status {
            NetworkStatus::Idle => "Ready",
            NetworkStatus::Connecting => "Connecting...",
            NetworkStatus::Connected => "Connected",
            NetworkStatus::Disconnected => "Disconnected - try again",
        };

        let field_labels = [
            format!(
                "Game ID: {}{}",
                self.network_game_id,
                if self.network_lobby_field == 0 { "_" } else { " " }
            ),
            format!("Player:  {}", self.network_player_select),
            "[ Connect ]".to_string(),
            "[ Back    ]".to_string(),
        ];

        let mut lines = vec![
            format!(" Status: {}\n", status_label),
            String::new(),
        ];
        for (i, label) in field_labels.iter().enumerate() {
            if i == self.network_lobby_field {
                lines.push(format!(" > {} <\n", label));
            } else {
                lines.push(format!("   {}\n", label));
            }
        }
        lines.push(String::new());
        lines.push(String::from(
            " Tab/↑↓ navigate  ←/→ toggle player  Enter confirm  Esc back",
        ));

        let popup = Paragraph::new(lines.concat())
            .block(
                Block::default()
                    .title(" Play Online ")
                    .borders(Borders::ALL)
                    .border_type(BorderType::Double)
                    .style(Style::default().fg(Color::Cyan)),
            )
            .style(Style::default().fg(Color::Green))
            .alignment(Alignment::Left);
        frame.render_widget(popup, popup_area);
    }

    fn handle_network_lobby_events(&mut self) -> io::Result<()> {
        if event::poll(Duration::from_millis(10))? {
            match event::read()? {
                Event::Key(key_event) if key_event.kind == KeyEventKind::Press => {
                    match key_event.code {
                        KeyCode::Esc => {
                            self.screen = AppScreen::MainMenu;
                        }
                        KeyCode::Tab | KeyCode::Down => {
                            self.network_lobby_field = (self.network_lobby_field + 1) % 4;
                        }
                        KeyCode::Up => {
                            if self.network_lobby_field == 0 {
                                self.network_lobby_field = 3;
                            } else {
                                self.network_lobby_field -= 1;
                            }
                        }
                        KeyCode::Left | KeyCode::Right => {
                            if self.network_lobby_field == 1 {
                                self.network_player_select = if self.network_player_select == 1 { 2 } else { 1 };
                            }
                        }
                        KeyCode::Backspace => {
                            if self.network_lobby_field == 0 {
                                self.network_game_id.pop();
                            }
                        }
                        KeyCode::Char(c) => {
                            if self.network_lobby_field == 0 {
                                if self.network_game_id.len() < 20 && c.is_ascii_alphanumeric() {
                                    self.network_game_id.push(c);
                                }
                            }
                        }
                        KeyCode::Enter => {
                            match self.network_lobby_field {
                                3 => {
                                    // Back
                                    self.screen = AppScreen::MainMenu;
                                }
                                2 | _ => {
                                    // Connect
                                    self.launch_network_game();
                                }
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn draw_game_over_overlay(frame: &mut Frame, game: &Game) {
        use helpers::centered_rect;

        let area = frame.area();
        let popup_area = centered_rect(50, 12, area.width, area.height);

        // Determine winner
        let (p1_score, p2_score) = game.get_scores();
        let winner_text = if p1_score > p2_score {
            "Player 1 Wins!"
        } else {
            "Player 2 Wins!"
        };

        let text = format!(
            "{}\n\n{} - {}\n\nPress SPACE to ready up\nEsc to quit",
            winner_text, p1_score, p2_score
        );

        let popup = Paragraph::new(text)
            .block(
                Block::default()
                    .title("Game Over")
                    .borders(Borders::ALL)
                    .border_type(BorderType::Thick)
                    .style(Style::default().fg(Color::Yellow).bg(Color::Black)),
            )
            .style(Style::default().fg(Color::White))
            .alignment(Alignment::Center);

        frame.render_widget(popup, popup_area);
    }

    fn launch_network_game(&mut self) {
        let game_id = if self.network_game_id.trim().is_empty() {
            "demo".to_string()
        } else {
            self.network_game_id.trim().to_string()
        };

        self.network_local_player = self.network_player_select;
        self.network_status = NetworkStatus::Connecting;

        let config = NetworkConfig {
            game_id: game_id.clone(),
            player: self.network_local_player,
            ..NetworkConfig::default()
        };

        let handle = network::connect(config);
        self.network_rx = Some(handle.rx);
        self.network_paddle_tx = Some(handle.paddle_tx);
        self.network_serve_tx = Some(handle.serve_tx);
        self.network_restart_tx = Some(handle.restart_tx);
        self.network_ready_tx = Some(handle.ready_tx);

        let p1_name = if self.network_local_player == 1 { "You" } else { "Opponent" };
        let p2_name = if self.network_local_player == 2 { "You" } else { "Opponent" };

        let mut game = Game::new(
            [p1_name, p2_name],
            Rect::default(),
            GameType::WithNetwork,
            Some(1.0),
        );
        game.set_theme(self.selected_theme);
        game.set_local_player_index((self.network_local_player - 1) as usize);
        self.current_game = Some(game);
        self.screen = AppScreen::Game;
    }

    // -----------------------------------------------------------------------
    // Network event processing (called each frame while in Game screen)
    // -----------------------------------------------------------------------

    fn drain_network_events(&mut self) {
        let local = self.network_local_player;
        let opponent_idx = if local == 1 { 1 } else { 0 };

        if let Some(rx) = &self.network_rx {
            // drain all pending events without blocking
            loop {
                match rx.try_recv() {
                    Ok(event) => match event {
                        NetworkEvent::Connected => {
                            self.network_status = NetworkStatus::Connected;
                        }
                        NetworkEvent::Disconnected => {
                            self.network_status = NetworkStatus::Disconnected;
                        }
                        NetworkEvent::OpponentPaddle(y) => {
                            if let Some(game) = &mut self.current_game {
                                game.set_opponent_paddle(opponent_idx, y);
                            }
                        }
                        NetworkEvent::BallUpdate(b) => {
                            if let Some(game) = &mut self.current_game {
                                game.set_ball_from_network(b.x, b.y, b.dx, b.dy);
                            }
                        }
                        NetworkEvent::StateUpdate(s) => {
                            if let Some(game) = &mut self.current_game {
                                game.set_scores(s.p1_score, s.p2_score);
                            }
                            // Track game over state for UI overlay
                            if s.status == network::GameStatus::Ended {
                                self.game_over = true;
                            } else if s.status == network::GameStatus::Playing {
                                self.game_over = false;
                            }
                        }
                    },
                    Err(_) => break, // channel empty or closed
                }
            }
        }
    }

    fn exit(&mut self) {
        self.exit = true;
    }
}

fn main() -> io::Result<()> {
    let terminal = ratatui::init();
    let mut app = App::new();

    let mut stdout = io::stdout();
    stdout.execute(event::EnableMouseCapture)?;

    let app_result = app.run(terminal);

    stdout.lock().execute(event::DisableMouseCapture)?;

    ratatui::restore();

    match &app_result {
        Ok(()) => {
            println!("Thanks for playing terminal.pong! 🏓");
            // println!(
            //     "Final Score: {} - {}",
            //     app.current_game.get_player(0).score,
            //     app.current_game.get_player(1).score
            // );
            if let Some(game) = app.current_game.as_ref() {
                // Display final scores
                println!(
                    "Final Score: {} - {}",
                    game.get_player(0).score,
                    game.get_player(1).score
                );
            }
        }
        Err(e) => {
            eprintln!("Game ended with error: {}", e);
        }
    }

    app_result
}
