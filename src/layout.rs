use evdev::KeyCode;

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    // Pressed together, in order.
    Key(Vec<KeyCode>),
    Command(String),
    ToggleStrip,
    Slider(Slider),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Slider {
    Brightness,
    Volume,
    Keyboard,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Key {
    pub label: String,
    pub action: Action,
}

// Slider value is 0.0 to 1.0, left is in pixels.
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Key(Key),
    Media,
    Slider { kind: Slider, value: f64, left: f64 },
}

pub type Groups = Vec<Vec<Item>>;

// strip is the Control Strip on the right. Empty when expanded.
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    pub groups: Groups,
    pub strip: Vec<Item>,
}

impl Layout {
    pub fn plates(&self) -> impl Iterator<Item = &[Item]> {
        let groups = self.groups.iter().map(Vec::as_slice);
        groups.chain(std::iter::once(self.strip.as_slice()))
    }

    pub fn items(&self) -> impl Iterator<Item = &Item> {
        self.plates().flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(label: &str) -> Item {
        Item::Key(Key {
            label: label.to_string(),
            action: Action::ToggleStrip,
        })
    }

    #[test]
    fn items_run_through_the_groups_then_the_strip() {
        let layout = Layout {
            groups: vec![vec![key("a")], vec![key("b"), Item::Media]],
            strip: vec![key("d")],
        };
        assert_eq!(layout.items().count(), 4);
        assert_eq!(layout.items().nth(2), Some(&Item::Media));
        assert_eq!(layout.plates().count(), 3);
    }
}
