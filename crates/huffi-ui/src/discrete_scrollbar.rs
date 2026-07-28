use iced::advanced::layout::{self, Layout, Limits};
use iced::advanced::renderer;
use iced::advanced::widget::Tree;
use iced::advanced::{Clipboard, Shell, Widget};
use iced::event;
use iced::mouse;
use iced::{Background, Border, Element, Length, Rectangle, Shadow, Size};

use crate::theme::MAUVE;

const BAR_WIDTH: f32 = 6.0;

pub struct DiscreteScrollbar<Message> {
    width: f32,
    selected: usize,
    total: usize,
    on_scroll: Option<Box<dyn Fn(usize) -> Message>>,
}

impl<Message> DiscreteScrollbar<Message> {
    pub fn new(selected: usize, total: usize) -> Self {
        Self {
            width: BAR_WIDTH,
            selected,
            total,
            on_scroll: None,
        }
    }

    pub fn on_scroll(mut self, f: impl Fn(usize) -> Message + 'static) -> Self {
        self.on_scroll = Some(Box::new(f));
        self
    }

    fn selected_from_y(&self, bounds: &Rectangle, y: f32) -> usize {
        if self.total == 0 {
            return 0;
        }
        let total_height = bounds.height;
        let bar_height = total_height / self.total as f32;
        let max_pos = (total_height - bar_height).max(0.0);
        if max_pos <= 0.0 {
            0
        } else {
            let bar_y = (y - bounds.y - bar_height * 0.5).clamp(0.0, max_pos);
            (bar_y / max_pos * (self.total - 1) as f32).round() as usize
        }
    }
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for DiscreteScrollbar<Message>
where
    Renderer: renderer::Renderer,
{
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fixed(self.width), Length::Fill)
    }

    fn layout(
        &self,
        _state: &mut Tree,
        _renderer: &Renderer,
        limits: &Limits,
    ) -> layout::Node {
        let size = limits.resolve(self.width, Length::Fill, Size::ZERO);
        layout::Node::new(size)
    }

    fn on_event(
        &mut self,
        _state: &mut Tree,
        event: event::Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) -> event::Status {
        if let event::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) = &event
            && let Some(pos) = cursor.position_in(layout.bounds())
            && let Some(ref on_scroll) = self.on_scroll
        {
            let new_selected = self.selected_from_y(&layout.bounds(), pos.y);
            shell.publish(on_scroll(new_selected));
            return event::Status::Captured;
        }
        event::Status::Ignored
    }

    fn draw(
        &self,
        _state: &Tree,
        renderer: &mut Renderer,
        _theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        if self.total == 0 {
            return;
        }
        let bounds = layout.bounds();
        let total_height = bounds.height;
        let bar_height = total_height / self.total as f32;
        let max_pos = (total_height - bar_height).max(0.0);
        let bar_y = if self.total <= 1 {
            0.0
        } else {
            self.selected as f32 / (self.total - 1) as f32 * max_pos
        };

        renderer.fill_quad(
            renderer::Quad {
                bounds: Rectangle {
                    x: bounds.x,
                    y: bounds.y + bar_y,
                    width: bounds.width,
                    height: bar_height,
                },
                border: Border::default().rounded(3.0),
                shadow: Shadow::default(),
            },
            Background::Color(MAUVE),
        );
    }
}

impl<'a, Message, Theme, Renderer> From<DiscreteScrollbar<Message>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Theme: 'a,
    Renderer: renderer::Renderer + 'a,
{
    fn from(scrollbar: DiscreteScrollbar<Message>) -> Self {
        Element::new(scrollbar)
    }
}
