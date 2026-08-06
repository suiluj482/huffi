mod daemon;
mod discrete_scrollbar;
mod theme;

use iced::keyboard::key::Named;
use iced::keyboard::{self, Event as KeyboardEvent};
use iced::widget::{button, column, row, container, horizontal_rule, mouse_area, text, text_input, Space};
use iced::widget::image as iced_image;
use iced::widget::svg as iced_svg;
use iced::{Element, Length, Subscription, Task as Command};
use iced_layershell::reexport::{Anchor, KeyboardInteractivity, Layer};
use iced_layershell::settings::{LayerShellSettings, Settings};
use iced_layershell::to_layer_message;
use iced_layershell::Application;
use std::time::{Duration, Instant};

use discrete_scrollbar::DiscreteScrollbar;

const PAGE_SIZE: usize = 10;
const KEY_REPEAT_INTERVAL: Duration = Duration::from_millis(80);
const DOUBLE_CLICK_INTERVAL: Duration = Duration::from_millis(300);

use theme::{BASE, ICON_SIZE, MAUVE, SUBTEXT0, SURFACE0, SURFACE1, TEXT};

fn icon_widget(path: &str) -> Element<'_, Message> {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    if ext.eq_ignore_ascii_case("svg") {
        iced_svg::Svg::from_path(path)
            .width(ICON_SIZE)
            .height(ICON_SIZE)
            .into()
    } else {
        iced_image::Image::new(iced_image::Handle::from_path(path))
            .width(ICON_SIZE)
            .height(ICON_SIZE)
            .into()
    }
}

pub fn main() -> Result<(), iced_layershell::Error> {
    let mut initial_query = String::new();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--query" | "-q" => {
                if let Some(query) = args.get(i + 1) {
                    initial_query = query.clone();
                    i += 2;
                } else {
                    eprintln!("error: --query requires an argument");
                    std::process::exit(1);
                }
            }
            "--help" | "-h" => {
                eprintln!("Usage: huffi-ui [--query <query>]");
                std::process::exit(0);
            }
            other => {
                eprintln!("error: unknown argument: {other}");
                std::process::exit(1);
            }
        }
    }

    HuffiApp::run(Settings {
        flags: initial_query,
        layer_settings: LayerShellSettings {
            anchor: Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right,
            layer: Layer::Overlay,
            keyboard_interactivity: KeyboardInteractivity::Exclusive,
            ..Default::default()
        },
        ..Default::default()
    })
}

struct HuffiApp {
    query: String,
    active_prefix: Option<String>,
    providers: Vec<huffi_protocol::ProviderEntry>,
    entries: Vec<huffi_protocol::QueryHit>,
    total: usize,
    selected: usize,
    last_click: Option<(usize, Instant)>,
    last_key_time: Instant,
    socket_path: std::path::PathBuf,
}

#[to_layer_message]
#[derive(Debug, Clone)]
enum Message {
    InputChanged(String),
    EntriesReceived { prefix: Option<String>, entries: Vec<huffi_protocol::QueryHit>, total: usize },
    ProvidersReceived { providers: Vec<huffi_protocol::ProviderEntry> },
    KeyPressed(KeyboardEvent),
    Boost(usize),
    Delete(usize),
    Scrolled(usize),
    RowPressed(usize),
    MouseWheel { y: f32 },
    Dismiss,
    IcedEvent(iced::Event),
}

impl Application for HuffiApp {
    type Executor = iced::executor::Default;
    type Message = Message;
    type Theme = iced::Theme;
    type Flags = String;

    fn new(initial_query: String) -> (Self, Command<Message>) {
        let socket_path = daemon::default_socket_path();
        let socket = socket_path.clone();
        let providers_socket = socket_path.clone();
        let query = initial_query.clone();
        (
            Self {
                query,
                active_prefix: None,
                providers: Vec::new(),
                entries: Vec::new(),
                total: 0,
                selected: 0,
                last_click: None,
                last_key_time: Instant::now(),
                socket_path,
            },
            Command::batch([
                text_input::focus("query"),
                Command::perform(
                    async move { daemon::query(&socket, &initial_query, 0, PAGE_SIZE).unwrap_or((None, Vec::new(), 0)) },
                    |(prefix, entries, total)| Message::EntriesReceived { prefix, entries, total },
                ),
                Command::perform(
                    async move { daemon::providers(&providers_socket).unwrap_or_default() },
                    |providers| Message::ProvidersReceived { providers },
                ),
            ]),
        )
    }

    fn namespace(&self) -> String {
        String::from("huffi")
    }

    fn subscription(&self) -> Subscription<Message> {
        iced::event::listen_with(|event, status, _window| match &event {
            iced::Event::Window(iced::window::Event::Unfocused) => Some(Message::Dismiss),
            iced::Event::Mouse(iced::mouse::Event::ButtonPressed { .. }) => {
                if status == iced::event::Status::Ignored {
                    Some(Message::Dismiss)
                } else {
                    None
                }
            }
            iced::Event::Keyboard(_) => Some(Message::IcedEvent(event.clone())),
            iced::Event::Mouse(iced::mouse::Event::WheelScrolled { delta }) => {
                let y = match delta {
                    iced::mouse::ScrollDelta::Lines { y, .. } => *y,
                    iced::mouse::ScrollDelta::Pixels { y, .. } => *y,
                };
                Some(Message::MouseWheel { y })
            }
            _ => None,
        })
    }

    fn update(&mut self, message: Message) -> Command<Message> {
        match message {
            Message::InputChanged(text) => {
                self.query = text;
                self.selected = 0;

                let query = self.query.clone();
                let socket = self.socket_path.clone();
                Command::perform(
                    async move { daemon::query(&socket, &query, 0, PAGE_SIZE).unwrap_or((None, Vec::new(), 0)) },
                    |(prefix, entries, total)| Message::EntriesReceived { prefix, entries, total },
                )
            }

            Message::EntriesReceived { prefix, entries, total } => {
                self.active_prefix = prefix;
                self.entries = entries;
                self.total = total;
                Command::none()
            }

            Message::ProvidersReceived { providers } => {
                self.providers = providers;
                Command::none()
            }

            Message::Boost(index) => {
                if let Some((_hit, key)) = self.entries.get(index).and_then(|h| h.history_key.as_ref().map(|k| (h, k))) {
                        let socket = self.socket_path.clone();
                        let query = self.query.clone();
                        let key = key.clone();
                        let offset = (self.selected / PAGE_SIZE) * PAGE_SIZE;
                        let q = self.query.clone();
                        let s = self.socket_path.clone();
                        return Command::perform(
                            async move {
                                let _ = daemon::boost(&socket, &query, &key);
                                daemon::query(&s, &q, offset, PAGE_SIZE).unwrap_or((None, Vec::new(), 0))
                            },
                            |(prefix, entries, total)| Message::EntriesReceived { prefix, entries, total },
                        );
                }
                Command::none()
            }

            Message::Delete(index) => {
                if let Some((_hit, key)) = self.entries.get(index).and_then(|h| h.history_key.as_ref().map(|k| (h, k))) {
                        let socket = self.socket_path.clone();
                        let query = self.query.clone();
                        let key = key.clone();
                        let offset = (self.selected / PAGE_SIZE) * PAGE_SIZE;
                        let q = self.query.clone();
                        let s = self.socket_path.clone();
                        return Command::perform(
                            async move {
                                let _ = daemon::delete(&socket, &query, &key);
                                daemon::query(&s, &q, offset, PAGE_SIZE).unwrap_or((None, Vec::new(), 0))
                            },
                            |(prefix, entries, total)| Message::EntriesReceived { prefix, entries, total },
                        );
                }
                Command::none()
            }

            Message::Scrolled(new_selected) => {
                if self.total == 0 {
                    return Command::none();
                }
                let old_page = self.page();
                self.selected = new_selected.min(self.total - 1);
                if self.page() != old_page {
                    self.fetch_page()
                } else {
                    Command::none()
                }
            }

            Message::RowPressed(index) => {
                if index >= self.total {
                    return Command::none();
                }
                let now = Instant::now();
                let is_double = matches!(
                    self.last_click,
                    Some((prev_index, prev_time))
                        if prev_index == index && now.duration_since(prev_time) < DOUBLE_CLICK_INTERVAL
                );
                self.last_click = Some((index, now));

                let old_page = self.page();
                self.selected = index;
                let cmd = if self.page() != old_page {
                    self.fetch_page()
                } else {
                    Command::none()
                };

                if is_double {
                    self.submit()
                } else {
                    cmd
                }
            }

            Message::MouseWheel { y } => {
                if self.total == 0 {
                    return Command::none();
                }
                let old_page = self.page();
                if y < 0.0 {
                    if self.selected + 1 < self.total {
                        self.selected += 1;
                    }
                } else if y > 0.0 && self.selected > 0 {
                    self.selected -= 1;
                }
                if self.page() != old_page {
                    self.fetch_page()
                } else {
                    Command::none()
                }
            }

            Message::KeyPressed(event) => {
                match event {
                    KeyboardEvent::KeyPressed {
                        key: keyboard::Key::Named(Named::ArrowDown),
                        ..
                    } => {
                        if self.entries.is_empty() || !self.should_accept_key() {
                            return Command::none();
                        }
                        let local = self.selected % PAGE_SIZE;
                        let old_page = self.page();
                        if local + 1 < self.entries.len() {
                            self.selected += 1;
                            Command::none()
                        } else if self.entries.len() == PAGE_SIZE {
                            self.selected += 1;
                            if self.page() != old_page {
                                self.fetch_page()
                            } else {
                                Command::none()
                            }
                        } else {
                            self.selected = 0;
                            if old_page != 0 {
                                self.fetch_page()
                            } else {
                                Command::none()
                            }
                        }
                    }

                    KeyboardEvent::KeyPressed {
                        key: keyboard::Key::Named(Named::ArrowUp),
                        ..
                    } => {
                        if self.entries.is_empty() || self.selected == 0 || !self.should_accept_key() {
                            return Command::none();
                        }
                        let local = self.selected % PAGE_SIZE;
                        let old_page = self.page();
                        if local > 0 {
                            self.selected -= 1;
                            Command::none()
                        } else {
                            self.selected -= 1;
                            if self.page() != old_page {
                                self.fetch_page()
                            } else {
                                Command::none()
                            }
                        }
                    }

                    KeyboardEvent::KeyPressed {
                        key: keyboard::Key::Named(Named::Enter),
                        ..
                    } => self.submit(),

                    KeyboardEvent::KeyPressed {
                        key: keyboard::Key::Named(Named::Escape),
                        ..
                    } => self.dismiss(),

                    KeyboardEvent::KeyPressed {
                        key: keyboard::Key::Named(Named::Tab),
                        ..
                    } => {
                        if self.entries.is_empty() || !self.should_accept_key() {
                            return Command::none();
                        }
                        let local = self.selected % PAGE_SIZE;
                        let old_page = self.page();
                        if local + 1 < self.entries.len() {
                            self.selected += 1;
                            Command::none()
                        } else if self.entries.len() == PAGE_SIZE {
                            self.selected += 1;
                            if self.page() != old_page {
                                self.fetch_page()
                            } else {
                                Command::none()
                            }
                        } else {
                            self.selected = 0;
                            if old_page != 0 {
                                self.fetch_page()
                            } else {
                                Command::none()
                            }
                        }
                    }

                    _ => Command::none(),
                }
            }

            Message::IcedEvent(event) => {
                if let iced::Event::Keyboard(key_event) = event {
                    return self.update(Message::KeyPressed(key_event));
                }
                Command::none()
            }

            Message::Dismiss => self.dismiss(),
            _ => Command::none(),
        }
    }

    fn view(&self) -> Element<'_, Message> {
        let input = text_input("Type to search...", &self.query)
            .id("query")
            .on_input(Message::InputChanged)
            .padding(12)
            .width(Length::Fill)
                    .style(|_theme, status| iced::widget::text_input::Style {
                background: SURFACE0.into(),
                border: iced::Border {
                    color: if matches!(status, iced::widget::text_input::Status::Focused) {
                        MAUVE
                    } else {
                        SURFACE1
                    },
                    width: 1.5,
                    ..Default::default()
                },
                icon: TEXT,
                placeholder: SUBTEXT0,
                value: TEXT,
                selection: MAUVE,
            });

        let mut list = column![].spacing(2).padding(4);
        let local = self.selected % PAGE_SIZE;
        for (i, hit) in self.entries.iter().enumerate() {
            let is_selected = i == local;
            let title_color = if is_selected { MAUVE } else { TEXT };
            let sub_color = if is_selected { MAUVE } else { SUBTEXT0 };

            let label: Element<'_, Message> = match &hit.subtitle {
                Some(sub) => {
                    let title = text(&hit.title).size(16).color(title_color);
                    let bracket = text(format!("({sub})"))
                        .size(12)
                        .color(sub_color)
                        .font(iced::Font {
                            style: iced::font::Style::Italic,
                            ..iced::Font::DEFAULT
                        });
                    row![title, bracket]
                        .spacing(12)
                        .align_y(iced::Alignment::Center)
                        .into()
                }
                None => text(&hit.title)
                    .width(Length::Fill)
                    .size(16)
                    .color(title_color)
                    .into(),
            };

            let icon: Element<'_, Message> = match hit.icon.as_deref() {
                Some(path) => icon_widget(path),
                None => Space::new(ICON_SIZE, ICON_SIZE).into(),
            };

            let row_content = {
                let mut right = row![].spacing(6).align_y(iced::Alignment::Center);
                right = right.push(text(format!("{:.2}", hit.base_score)).size(12).color(SUBTEXT0));
                if let Some(h) = hit.history_score {
                    right = right.push(text(format!("{h:.2}")).size(12).color(SUBTEXT0));
                }
                if hit.history_key.is_some() {
                    let boost = button(text("+").size(12))
                        .on_press(Message::Boost(i))
                        .padding([0, 4])
                        .style(|_theme, _status| iced::widget::button::Style {
                            text_color: SUBTEXT0,
                            background: None,
                            border: iced::Border::default(),
                            shadow: iced::Shadow::default(),
                        });
                    let del = button(text("\u{2212}").size(12))
                        .on_press(Message::Delete(i))
                        .padding([0, 4])
                        .style(|_theme, _status| iced::widget::button::Style {
                            text_color: SUBTEXT0,
                            background: None,
                            border: iced::Border::default(),
                            shadow: iced::Shadow::default(),
                        });
                    right = right.push(boost).push(del);
                }
                row![icon, label, Space::new(Length::Fill, 1), right]
                    .spacing(8)
                    .align_y(iced::Alignment::Center)
            };

            let row =
                if is_selected {
                    container(row_content)
                        .width(Length::Fill)
                        .padding(6)
                        .style(|_theme| iced::widget::container::Style {
                            background: Some(SURFACE1.into()),
                            text_color: Some(MAUVE),
                            ..Default::default()
                        })
                } else {
                    container(row_content)
                        .width(Length::Fill)
                        .padding(6)
                        .style(|_theme| iced::widget::container::Style {
                            background: Some(SURFACE0.into()),
                            ..Default::default()
                        })
                };

            let global_index = self.page() * PAGE_SIZE + i;
            list = list.push(
                mouse_area(row)
                    .interaction(iced::mouse::Interaction::Pointer)
                    .on_press(Message::RowPressed(global_index)),
            );
        }

        let rail: Element<'_, Message> = if self.total == 0 {
            Space::new(Length::Fixed(6.0), Length::Fill).into()
        } else {
            DiscreteScrollbar::new(self.selected, self.total)
                .on_scroll(Message::Scrolled)
                .into()
        };

        let content = column![
            row![input, self.prefix_badge()].spacing(8).align_y(iced::Alignment::Center),
            horizontal_rule(1),
            row![list, rail],
            horizontal_rule(1),
            self.provider_footer(),
        ]
        .spacing(0)
        .width(Length::Fill)
        .height(Length::Fill);

        let panel = container(content)
            .width(640)
            .height(480)
            .padding(8)
            .style(|_theme| iced::widget::container::Style {
                background: Some(BASE.into()),
                ..Default::default()
            });

        container(panel)
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into()
    }

    fn style(&self, _theme: &iced::Theme) -> iced_layershell::Appearance {
        iced_layershell::Appearance {
            background_color: iced::Color::TRANSPARENT,
            text_color: TEXT,
        }
    }
}

impl HuffiApp {
    fn page(&self) -> usize {
        self.selected / PAGE_SIZE
    }

    fn should_accept_key(&mut self) -> bool {
        if self.last_key_time.elapsed() >= KEY_REPEAT_INTERVAL {
            self.last_key_time = Instant::now();
            true
        } else {
            false
        }
    }

    fn fetch_page(&self) -> Command<Message> {
        let query = self.query.clone();
        let socket = self.socket_path.clone();
        let offset = self.page() * PAGE_SIZE;
        Command::perform(
            async move {
                daemon::query(&socket, &query, offset, PAGE_SIZE)
                    .unwrap_or((None, Vec::new(), 0))
            },
            |(prefix, entries, total)| Message::EntriesReceived { prefix, entries, total },
        )
    }

    fn prefix_badge(&self) -> Element<'_, Message> {
        let Some(prefix) = self.active_prefix.as_deref() else {
            return Space::new(Length::Shrink, 1).into();
        };

        let provider = self
            .providers
            .iter()
            .find(|p| p.prefixes.iter().any(|pfx| pfx == prefix));
        let label = match provider {
            Some(p) => format!("{prefix}  {}", p.id),
            None => prefix.to_string(),
        };

        container(text(label).size(12).color(MAUVE))
            .padding([4, 8])
            .style(|_theme| iced::widget::container::Style {
                background: Some(SURFACE1.into()),
                border: iced::Border {
                    color: MAUVE,
                    width: 1.0,
                    radius: 6.0.into(),
                },
                ..Default::default()
            })
            .into()
    }

    fn provider_footer(&self) -> Element<'_, Message> {
        if self.providers.is_empty() {
            return Space::new(Length::Fill, 1).into();
        }

        let parts: Vec<String> = self
            .providers
            .iter()
            .map(|p| {
                if p.prefixes.is_empty() {
                    p.id.clone()
                } else {
                    format!("{}: {}", p.id, p.prefixes.join(", "))
                }
            })
            .collect();

        text(parts.join("  ·  "))
            .size(11)
            .color(SUBTEXT0)
            .width(Length::Fill)
            .into()
    }

    fn submit(&mut self) -> Command<Message> {
        let local = self.selected % PAGE_SIZE;
        if let Some(hit) = self.entries.get(local) {
            let socket = self.socket_path.clone();
            let query = self.query.clone();
            let entry_id = hit.entry_id.clone();
            let _ = daemon::select(&socket, &query, &entry_id);
            self.dismiss()
        } else {
            Command::none()
        }
    }

    fn dismiss(&self) -> Command<Message> {
        std::process::exit(0);
    }
}
