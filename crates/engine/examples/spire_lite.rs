use std::sync::Mutex;

use egui::{Align, Align2, Color32, Frame, Id, Layout, RichText, Stroke, Vec2};
use engine::ecs::World;
use engine::glam::{Quat, Vec3};
use engine::time::Time;
use engine::{
    AmbientLight, App, Camera3D, DirectionalLight, GlobalTransform, Material, Mesh, Query, Res,
    ResMut, Transform, UiSystem, Vertex,
};

const STARTING_HP: i32 = 70;
const MAX_FLOOR: usize = 3;
const HAND_SIZE: usize = 5;
const STARTING_ENERGY: i32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CardKind {
    Strike,
    Defend,
    Bash,
    QuickShot,
    IronSkin,
    HeavyBlow,
    Mend,
}

#[derive(Debug, Clone, Copy)]
struct CardInfo {
    name: &'static str,
    cost: i32,
    text: &'static str,
    accent: Color32,
}

impl CardKind {
    fn info(self) -> CardInfo {
        match self {
            Self::Strike => CardInfo {
                name: "Strike",
                cost: 1,
                text: "Deal 6 damage.",
                accent: Color32::from_rgb(190, 72, 62),
            },
            Self::Defend => CardInfo {
                name: "Defend",
                cost: 1,
                text: "Gain 5 block.",
                accent: Color32::from_rgb(68, 118, 190),
            },
            Self::Bash => CardInfo {
                name: "Bash",
                cost: 2,
                text: "Deal 8. Apply 2 vulnerable.",
                accent: Color32::from_rgb(196, 138, 55),
            },
            Self::QuickShot => CardInfo {
                name: "Quick Shot",
                cost: 1,
                text: "Deal 4. Draw 1 card.",
                accent: Color32::from_rgb(73, 151, 142),
            },
            Self::IronSkin => CardInfo {
                name: "Iron Skin",
                cost: 2,
                text: "Gain 12 block.",
                accent: Color32::from_rgb(94, 126, 152),
            },
            Self::HeavyBlow => CardInfo {
                name: "Heavy Blow",
                cost: 2,
                text: "Deal 14 damage.",
                accent: Color32::from_rgb(160, 82, 77),
            },
            Self::Mend => CardInfo {
                name: "Mend",
                cost: 1,
                text: "Heal 4 HP.",
                accent: Color32::from_rgb(85, 152, 94),
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EnemyIntent {
    Attack(i32),
    Block(i32),
    Strength(i32),
}

impl EnemyIntent {
    fn label(self) -> String {
        match self {
            Self::Attack(amount) => format!("Attack {amount}"),
            Self::Block(amount) => format!("Block {amount}"),
            Self::Strength(amount) => format!("Power +{amount}"),
        }
    }
}

#[derive(Debug, Clone)]
struct EnemyState {
    name: &'static str,
    hp: i32,
    max_hp: i32,
    block: i32,
    strength: i32,
    vulnerable: i32,
    intent: EnemyIntent,
    intent_step: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GamePhase {
    PlayerTurn,
    Reward,
    RunVictory,
    Defeat,
}

#[derive(Debug)]
struct CombatGame {
    player_hp: i32,
    max_player_hp: i32,
    player_block: i32,
    energy: i32,
    floor: usize,
    turn: usize,
    phase: GamePhase,
    deck: Vec<CardKind>,
    draw_pile: Vec<CardKind>,
    hand: Vec<CardKind>,
    discard_pile: Vec<CardKind>,
    enemy: EnemyState,
    rewards: Vec<CardKind>,
    rng: u32,
    log: Vec<String>,
}

impl CombatGame {
    fn new() -> Self {
        let mut game = Self {
            player_hp: STARTING_HP,
            max_player_hp: STARTING_HP,
            player_block: 0,
            energy: STARTING_ENERGY,
            floor: 1,
            turn: 0,
            phase: GamePhase::PlayerTurn,
            deck: starting_deck(),
            draw_pile: Vec::new(),
            hand: Vec::new(),
            discard_pile: Vec::new(),
            enemy: enemy_for_floor(1),
            rewards: Vec::new(),
            rng: 0x5EED_1234,
            log: Vec::new(),
        };
        game.start_fight();
        game
    }

    fn restart(&mut self) {
        *self = Self::new();
    }

    fn start_fight(&mut self) {
        self.enemy = enemy_for_floor(self.floor);
        self.draw_pile = self.deck.clone();
        shuffle_cards(&mut self.draw_pile, &mut self.rng);
        self.hand.clear();
        self.discard_pile.clear();
        self.rewards.clear();
        self.turn = 0;
        self.set_next_enemy_intent();
        self.start_player_turn();
        self.push_log(format!(
            "Floor {}: {} appears.",
            self.floor, self.enemy.name
        ));
    }

    fn start_player_turn(&mut self) {
        self.phase = GamePhase::PlayerTurn;
        self.turn += 1;
        self.energy = STARTING_ENERGY;
        self.player_block = 0;
        self.draw_cards(HAND_SIZE);
        self.push_log(format!("Turn {} begins.", self.turn));
    }

    fn play_card(&mut self, hand_index: usize) {
        if self.phase != GamePhase::PlayerTurn {
            return;
        }
        let Some(&card) = self.hand.get(hand_index) else {
            return;
        };
        let info = card.info();
        if info.cost > self.energy {
            self.push_log(format!("Not enough energy for {}.", info.name));
            return;
        }

        self.energy -= info.cost;
        let card = self.hand.remove(hand_index);
        self.apply_card(card);
        if matches!(self.phase, GamePhase::PlayerTurn) {
            self.discard_pile.push(card);
        }
    }

    fn apply_card(&mut self, card: CardKind) {
        match card {
            CardKind::Strike => self.damage_enemy(6),
            CardKind::Defend => self.gain_block(5),
            CardKind::Bash => {
                self.damage_enemy(8);
                self.enemy.vulnerable = self.enemy.vulnerable.max(2);
                self.push_log("Enemy is vulnerable.".to_string());
            }
            CardKind::QuickShot => {
                self.damage_enemy(4);
                self.draw_cards(1);
            }
            CardKind::IronSkin => self.gain_block(12),
            CardKind::HeavyBlow => self.damage_enemy(14),
            CardKind::Mend => {
                self.player_hp = (self.player_hp + 4).min(self.max_player_hp);
                self.push_log("Recovered 4 HP.".to_string());
            }
        }

        if self.enemy.hp <= 0 {
            self.win_fight();
        }
    }

    fn end_turn(&mut self) {
        if self.phase != GamePhase::PlayerTurn {
            return;
        }
        self.discard_pile.append(&mut self.hand);
        self.enemy_turn();
    }

    fn choose_reward(&mut self, reward_index: usize) {
        if self.phase != GamePhase::Reward {
            return;
        }
        let Some(&card) = self.rewards.get(reward_index) else {
            return;
        };
        self.deck.push(card);
        self.push_log(format!("Added {} to the deck.", card.info().name));
        self.floor += 1;
        self.start_fight();
    }

    fn enemy_turn(&mut self) {
        match self.enemy.intent {
            EnemyIntent::Attack(amount) => {
                let incoming = amount + self.enemy.strength;
                let blocked = incoming.min(self.player_block);
                self.player_block -= blocked;
                let damage = incoming - blocked;
                self.player_hp -= damage;
                self.push_log(format!(
                    "{} attacks for {incoming}. You take {damage}.",
                    self.enemy.name
                ));
            }
            EnemyIntent::Block(amount) => {
                self.enemy.block += amount;
                self.push_log(format!("{} gains {amount} block.", self.enemy.name));
            }
            EnemyIntent::Strength(amount) => {
                self.enemy.strength += amount;
                self.push_log(format!("{} grows stronger.", self.enemy.name));
            }
        }

        if self.player_hp <= 0 {
            self.player_hp = 0;
            self.phase = GamePhase::Defeat;
            self.push_log("The run ended.".to_string());
            return;
        }

        if self.enemy.vulnerable > 0 {
            self.enemy.vulnerable -= 1;
        }
        self.set_next_enemy_intent();
        self.start_player_turn();
    }

    fn damage_enemy(&mut self, base_damage: i32) {
        let damage = if self.enemy.vulnerable > 0 {
            base_damage * 3 / 2
        } else {
            base_damage
        };
        let blocked = damage.min(self.enemy.block);
        self.enemy.block -= blocked;
        let dealt = damage - blocked;
        self.enemy.hp -= dealt;
        self.push_log(format!("Dealt {dealt} damage."));
    }

    fn gain_block(&mut self, amount: i32) {
        self.player_block += amount;
        self.push_log(format!("Gained {amount} block."));
    }

    fn draw_cards(&mut self, amount: usize) {
        for _ in 0..amount {
            if self.draw_pile.is_empty() && !self.discard_pile.is_empty() {
                self.draw_pile.append(&mut self.discard_pile);
                shuffle_cards(&mut self.draw_pile, &mut self.rng);
                self.push_log("Shuffled discard into draw pile.".to_string());
            }
            let Some(card) = self.draw_pile.pop() else {
                return;
            };
            self.hand.push(card);
        }
    }

    fn set_next_enemy_intent(&mut self) {
        let step = self.enemy.intent_step;
        self.enemy.intent_step += 1;
        self.enemy.intent = match self.floor {
            1 => match step % 3 {
                0 => EnemyIntent::Attack(6),
                1 => EnemyIntent::Block(5),
                _ => EnemyIntent::Attack(7),
            },
            2 => match step % 4 {
                0 => EnemyIntent::Strength(2),
                1 | 2 => EnemyIntent::Attack(8),
                _ => EnemyIntent::Block(8),
            },
            _ => match step % 4 {
                0 => EnemyIntent::Attack(10),
                1 => EnemyIntent::Block(10),
                2 => EnemyIntent::Attack(12),
                _ => EnemyIntent::Strength(3),
            },
        };
    }

    fn win_fight(&mut self) {
        self.enemy.hp = 0;
        self.hand.clear();
        self.discard_pile.clear();
        if self.floor >= MAX_FLOOR {
            self.phase = GamePhase::RunVictory;
            self.push_log("The final enemy falls.".to_string());
        } else {
            self.phase = GamePhase::Reward;
            self.rewards = self.generate_rewards();
            self.push_log("Choose a card reward.".to_string());
        }
    }

    fn generate_rewards(&mut self) -> Vec<CardKind> {
        const POOL: [CardKind; 5] = [
            CardKind::QuickShot,
            CardKind::IronSkin,
            CardKind::HeavyBlow,
            CardKind::Mend,
            CardKind::Bash,
        ];
        let mut rewards = Vec::with_capacity(3);
        let offset = (next_rng(&mut self.rng) as usize) % POOL.len();
        for i in 0..3 {
            rewards.push(POOL[(offset + i) % POOL.len()]);
        }
        rewards
    }

    fn push_log(&mut self, line: String) {
        self.log.push(line);
        if self.log.len() > 8 {
            self.log.remove(0);
        }
    }
}

fn starting_deck() -> Vec<CardKind> {
    let mut deck = Vec::new();
    deck.extend(std::iter::repeat_n(CardKind::Strike, 5));
    deck.extend(std::iter::repeat_n(CardKind::Defend, 4));
    deck.push(CardKind::Bash);
    deck
}

fn enemy_for_floor(floor: usize) -> EnemyState {
    match floor {
        1 => EnemyState {
            name: "Training Slime",
            hp: 32,
            max_hp: 32,
            block: 0,
            strength: 0,
            vulnerable: 0,
            intent: EnemyIntent::Attack(6),
            intent_step: 0,
        },
        2 => EnemyState {
            name: "Cult Acolyte",
            hp: 46,
            max_hp: 46,
            block: 0,
            strength: 0,
            vulnerable: 0,
            intent: EnemyIntent::Strength(2),
            intent_step: 0,
        },
        _ => EnemyState {
            name: "Bronze Warden",
            hp: 68,
            max_hp: 68,
            block: 0,
            strength: 0,
            vulnerable: 0,
            intent: EnemyIntent::Attack(10),
            intent_step: 0,
        },
    }
}

fn next_rng(seed: &mut u32) -> u32 {
    *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
    *seed
}

fn shuffle_cards(cards: &mut [CardKind], seed: &mut u32) {
    for i in (1..cards.len()).rev() {
        let j = (next_rng(seed) as usize) % (i + 1);
        cards.swap(i, j);
    }
}

#[derive(Default)]
struct UiActionQueue {
    actions: Mutex<Vec<UiAction>>,
}

impl UiActionQueue {
    fn push(&self, action: UiAction) {
        self.actions
            .lock()
            .expect("UI action queue mutex must not be poisoned")
            .push(action);
    }

    fn drain(&self) -> Vec<UiAction> {
        self.actions
            .lock()
            .expect("UI action queue mutex must not be poisoned")
            .drain(..)
            .collect()
    }
}

#[derive(Debug, Clone, Copy)]
enum UiAction {
    PlayCard(usize),
    EndTurn,
    ChooseReward(usize),
    Restart,
}

fn combat_action_system(mut game: ResMut<CombatGame>, actions: Res<UiActionQueue>) {
    for action in actions.drain() {
        match action {
            UiAction::PlayCard(index) => game.play_card(index),
            UiAction::EndTurn => game.end_turn(),
            UiAction::ChooseReward(index) => game.choose_reward(index),
            UiAction::Restart => game.restart(),
        }
    }
}

#[derive(Clone, Copy)]
enum FigureKind {
    Player,
    Enemy,
}

struct Figure {
    kind: FigureKind,
}

fn figure_update_system(
    time: Res<Time>,
    game: Res<CombatGame>,
    mut query: Query<(&Figure, &mut Transform, &mut Material)>,
) {
    let pulse = (time.elapsed_seconds * 2.5).sin() * 0.04;
    for (_, (figure, transform, material)) in &mut query {
        match figure.kind {
            FigureKind::Player => {
                transform.scale = Vec3::new(1.0 + pulse, 1.0 + pulse, 1.0 + pulse);
                transform.rotation = Quat::from_rotation_y(-0.25 + pulse);
                material.color = if game.player_block > 0 {
                    [0.28, 0.55, 0.95, 1.0]
                } else {
                    [0.18, 0.35, 0.82, 1.0]
                };
            }
            FigureKind::Enemy => {
                let hp_ratio = if game.enemy.max_hp > 0 {
                    game.enemy.hp.max(0) as f32 / game.enemy.max_hp as f32
                } else {
                    0.0
                };
                transform.scale = Vec3::new(1.0, 0.45 + hp_ratio * 0.75, 1.0);
                transform.translation.y = transform.scale.y * 0.55;
                transform.rotation = Quat::from_rotation_y(0.25 - pulse);
                material.color = if game.enemy.vulnerable > 0 {
                    [0.95, 0.55, 0.25, 1.0]
                } else {
                    [0.78, 0.18, 0.16, 1.0]
                };
            }
        }
    }
}

struct CardBattleHud;

impl UiSystem for CardBattleHud {
    fn run(&mut self, ctx: &egui::Context, world: &World) {
        let Some(game) = world.get_resource::<CombatGame>() else {
            return;
        };
        let Some(actions) = world.get_resource::<UiActionQueue>() else {
            return;
        };

        set_game_visuals(ctx);
        show_top_bar(ctx, game);
        show_log(ctx, game);
        show_hand(ctx, game, actions);
        show_phase_overlay(ctx, game, actions);
    }
}

fn set_game_visuals(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = Color32::from_rgba_premultiplied(16, 18, 22, 220);
    visuals.window_fill = Color32::from_rgb(24, 26, 31);
    visuals.widgets.inactive.bg_fill = Color32::from_rgb(42, 46, 54);
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(58, 64, 74);
    visuals.widgets.active.bg_fill = Color32::from_rgb(74, 82, 94);
    ctx.set_visuals(visuals);
}

fn show_top_bar(ctx: &egui::Context, game: &CombatGame) {
    let rect = ctx.content_rect();
    egui::Area::new(Id::new("spire_lite_top_bar"))
        .fixed_pos(rect.left_top())
        .show(ctx, |ui| {
            Frame::new()
                .fill(Color32::from_rgba_premultiplied(16, 18, 22, 230))
                .inner_margin(egui::Margin::same(10))
                .show(ui, |ui| {
                    ui.set_min_size(Vec2::new(rect.width(), 58.0));
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.heading(RichText::new("Spire Lite").strong());
                            ui.label(format!("Floor {} / {}", game.floor, MAX_FLOOR));
                        });
                        ui.add_space(24.0);
                        stat_block(
                            ui,
                            "Player",
                            game.player_hp,
                            game.max_player_hp,
                            game.player_block,
                        );
                        ui.add_space(18.0);
                        stat_block(
                            ui,
                            game.enemy.name,
                            game.enemy.hp,
                            game.enemy.max_hp,
                            game.enemy.block,
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(
                                RichText::new(game.enemy.intent.label()).color(Color32::LIGHT_RED),
                            );
                            ui.label(format!("Energy: {}", game.energy));
                        });
                    });
                });
        });
}

fn stat_block(ui: &mut egui::Ui, name: &str, hp: i32, max_hp: i32, block: i32) {
    ui.vertical(|ui| {
        ui.label(RichText::new(name).strong());
        let ratio = if max_hp > 0 {
            hp.max(0) as f32 / max_hp as f32
        } else {
            0.0
        };
        ui.add(
            egui::ProgressBar::new(ratio)
                .desired_width(190.0)
                .text(format!("HP {}/{}", hp.max(0), max_hp)),
        );
        ui.label(format!("Block: {block}"));
    });
}

fn show_log(ctx: &egui::Context, game: &CombatGame) {
    egui::Area::new(Id::new("spire_lite_log"))
        .anchor(Align2::RIGHT_TOP, Vec2::new(-12.0, 92.0))
        .show(ctx, |ui| {
            Frame::new()
                .fill(Color32::from_rgba_premultiplied(20, 22, 27, 224))
                .inner_margin(egui::Margin::same(10))
                .show(ui, |ui| {
                    ui.set_min_width(240.0);
                    ui.heading("Combat Log");
                    ui.separator();
                    for line in game.log.iter().rev() {
                        ui.label(line);
                    }
                });
        });
}

fn show_hand(ctx: &egui::Context, game: &CombatGame, actions: &UiActionQueue) {
    let rect = ctx.content_rect();
    egui::Area::new(Id::new("spire_lite_hand"))
        .anchor(Align2::LEFT_BOTTOM, Vec2::ZERO)
        .show(ctx, |ui| {
            Frame::new()
                .fill(Color32::from_rgba_premultiplied(16, 18, 22, 232))
                .inner_margin(egui::Margin::same(12))
                .show(ui, |ui| {
                    ui.set_min_size(Vec2::new(rect.width(), 196.0));
                    ui.horizontal_wrapped(|ui| {
                        for (index, card) in game.hand.iter().copied().enumerate() {
                            let playable = game.phase == GamePhase::PlayerTurn
                                && card.info().cost <= game.energy;
                            if draw_card_button(ui, card, playable).clicked() {
                                actions.push(UiAction::PlayCard(index));
                            }
                        }

                        ui.add_space(16.0);
                        let enabled = game.phase == GamePhase::PlayerTurn;
                        if ui
                            .add_enabled(
                                enabled,
                                egui::Button::new(RichText::new("End Turn").strong())
                                    .min_size(Vec2::new(120.0, 60.0)),
                            )
                            .clicked()
                        {
                            actions.push(UiAction::EndTurn);
                        }
                    });
                });
        });
}

fn draw_card_button(ui: &mut egui::Ui, card: CardKind, playable: bool) -> egui::Response {
    let info = card.info();
    let body = format!("{}\nCost {}\n\n{}", info.name, info.cost, info.text);
    let fill = if playable {
        info.accent
    } else {
        Color32::from_rgb(52, 54, 58)
    };
    ui.add_enabled(
        playable,
        egui::Button::new(RichText::new(body).size(14.0).color(Color32::WHITE))
            .fill(fill)
            .stroke(Stroke::new(1.0_f32, Color32::from_rgb(220, 220, 210)))
            .min_size(Vec2::new(142.0, 170.0)),
    )
}

fn show_phase_overlay(ctx: &egui::Context, game: &CombatGame, actions: &UiActionQueue) {
    match game.phase {
        GamePhase::Reward => show_reward_overlay(ctx, game, actions),
        GamePhase::RunVictory => {
            show_end_overlay(ctx, "Run Complete", "The spire is quiet.", actions)
        }
        GamePhase::Defeat => show_end_overlay(ctx, "Defeat", "The run is over.", actions),
        GamePhase::PlayerTurn => {}
    }
}

fn show_reward_overlay(ctx: &egui::Context, game: &CombatGame, actions: &UiActionQueue) {
    egui::Area::new(Id::new("spire_lite_reward"))
        .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
        .show(ctx, |ui| {
            Frame::popup(ui.style()).show(ui, |ui| {
                ui.heading("Card Reward");
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    for (index, card) in game.rewards.iter().copied().enumerate() {
                        if draw_card_button(ui, card, true).clicked() {
                            actions.push(UiAction::ChooseReward(index));
                        }
                    }
                });
            });
        });
}

fn show_end_overlay(ctx: &egui::Context, title: &str, body: &str, actions: &UiActionQueue) {
    egui::Area::new(Id::new("spire_lite_end"))
        .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
        .show(ctx, |ui| {
            Frame::popup(ui.style()).show(ui, |ui| {
                ui.heading(title);
                ui.label(body);
                ui.add_space(12.0);
                if ui.button("Restart").clicked() {
                    actions.push(UiAction::Restart);
                }
            });
        });
}

fn make_box_mesh(hx: f32, hy: f32, hz: f32, color: [f32; 3]) -> Mesh {
    let face_defs: [([f32; 3], [[f32; 3]; 4]); 6] = [
        (
            [1.0, 0.0, 0.0],
            [[hx, -hy, -hz], [hx, hy, -hz], [hx, hy, hz], [hx, -hy, hz]],
        ),
        (
            [-1.0, 0.0, 0.0],
            [
                [-hx, -hy, hz],
                [-hx, hy, hz],
                [-hx, hy, -hz],
                [-hx, -hy, -hz],
            ],
        ),
        (
            [0.0, 1.0, 0.0],
            [[-hx, hy, -hz], [hx, hy, -hz], [hx, hy, hz], [-hx, hy, hz]],
        ),
        (
            [0.0, -1.0, 0.0],
            [
                [-hx, -hy, hz],
                [hx, -hy, hz],
                [hx, -hy, -hz],
                [-hx, -hy, -hz],
            ],
        ),
        (
            [0.0, 0.0, 1.0],
            [[hx, -hy, hz], [hx, hy, hz], [-hx, hy, hz], [-hx, -hy, hz]],
        ),
        (
            [0.0, 0.0, -1.0],
            [
                [-hx, -hy, -hz],
                [-hx, hy, -hz],
                [hx, hy, -hz],
                [hx, -hy, -hz],
            ],
        ),
    ];

    let corner_uvs = [[0.0_f32, 1.0], [0.0, 0.0], [1.0, 0.0], [1.0, 1.0]];
    let mut vertices = Vec::with_capacity(24);
    let mut indices = Vec::with_capacity(36);

    for (face_index, (normal, positions)) in face_defs.iter().enumerate() {
        let base = (face_index * 4) as u32;
        for vertex_index in 0..4 {
            vertices.push(Vertex {
                position: positions[vertex_index],
                normal: *normal,
                color,
                uv: corner_uvs[vertex_index],
                outline_scale: 1.0,
                additional_uv: [0.0; 2],
            });
        }
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    Mesh {
        vertices,
        indices: Some(indices),
        skinning: None,
        tangents: None,
        submeshes: Vec::new(),
    }
}

fn make_ground_mesh(width: f32, depth: f32, color: [f32; 3]) -> Mesh {
    let hx = width * 0.5;
    let hz = depth * 0.5;
    let normal = [0.0_f32, 1.0, 0.0];
    Mesh {
        vertices: vec![
            Vertex {
                position: [-hx, 0.0, -hz],
                normal,
                color,
                uv: [0.0, 0.0],
                outline_scale: 1.0,
                additional_uv: [0.0; 2],
            },
            Vertex {
                position: [hx, 0.0, -hz],
                normal,
                color,
                uv: [1.0, 0.0],
                outline_scale: 1.0,
                additional_uv: [0.0; 2],
            },
            Vertex {
                position: [hx, 0.0, hz],
                normal,
                color,
                uv: [1.0, 1.0],
                outline_scale: 1.0,
                additional_uv: [0.0; 2],
            },
            Vertex {
                position: [-hx, 0.0, hz],
                normal,
                color,
                uv: [0.0, 1.0],
                outline_scale: 1.0,
                additional_uv: [0.0; 2],
            },
        ],
        indices: Some(vec![0, 1, 2, 0, 2, 3]),
        skinning: None,
        tangents: None,
        submeshes: Vec::new(),
    }
}

fn spawn_box(world: &mut World, translation: Vec3, mesh: Mesh, material: Material, figure: Figure) {
    let entity = world.spawn().expect("entity spawn must succeed");
    world
        .add_component(entity, Transform::from_translation(translation))
        .expect("transform must insert");
    world
        .add_component(entity, GlobalTransform::default())
        .expect("global transform must insert");
    world.add_component(entity, mesh).expect("mesh must insert");
    world
        .add_component(entity, material)
        .expect("material must insert");
    world
        .add_component(entity, figure)
        .expect("figure marker must insert");
}

fn setup_scene(world: &mut World) {
    world.insert_resource(AmbientLight {
        color: Vec3::new(0.8, 0.9, 1.0),
        intensity: 0.25,
    });
    world.insert_resource(DirectionalLight {
        direction: Vec3::new(-0.4, -1.0, -0.6).normalize(),
        color: Vec3::new(1.0, 0.96, 0.88),
        intensity: 1.6,
    });

    let ground = world.spawn().expect("ground spawn must succeed");
    world
        .add_component(ground, Transform::default())
        .expect("ground transform must insert");
    world
        .add_component(ground, GlobalTransform::default())
        .expect("ground global transform must insert");
    world
        .add_component(ground, make_ground_mesh(16.0, 10.0, [0.12, 0.16, 0.18]))
        .expect("ground mesh must insert");
    world
        .add_component(ground, Material::color(0.12, 0.16, 0.18))
        .expect("ground material must insert");

    spawn_box(
        world,
        Vec3::new(-3.0, 0.6, 0.0),
        make_box_mesh(0.55, 0.65, 0.55, [0.18, 0.35, 0.82]),
        Material::color(0.18, 0.35, 0.82),
        Figure {
            kind: FigureKind::Player,
        },
    );
    spawn_box(
        world,
        Vec3::new(3.0, 0.8, 0.0),
        make_box_mesh(0.7, 0.85, 0.7, [0.78, 0.18, 0.16]),
        Material::color(0.78, 0.18, 0.16),
        Figure {
            kind: FigureKind::Enemy,
        },
    );

    let camera = world.spawn().expect("camera spawn must succeed");
    world
        .add_component(
            camera,
            Transform::looking_at(Vec3::new(0.0, 6.0, 8.0), Vec3::new(0.0, 0.5, 0.0), Vec3::Y),
        )
        .expect("camera transform must insert");
    world
        .add_component(camera, GlobalTransform::default())
        .expect("camera global transform must insert");
    world
        .add_component(camera, Camera3D::default())
        .expect("camera must insert");
}

fn main() {
    let mut app = App::new().with_title("Spire Lite").with_size(1280, 720);

    {
        let world = app.world_mut();
        world.insert_resource(CombatGame::new());
        world.insert_resource(UiActionQueue::default());
        setup_scene(world);
    }

    app.add_system(combat_action_system);
    app.add_system(figure_update_system);
    app.add_ui_system(CardBattleHud);
    app.run().expect("event loop must run");
}
