use evdev::KeyCode;

// What happens when a key is tapped.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Key(KeyCode),
    Command(String),
    // Opens or closes the Control Strip.
    ToggleStrip,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Key {
    pub label: String,
    pub action: Action,
}

// One slot in the bar: a key, or Apple's now-playing widget.
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Key(Key),
    Media,
}

// Items in a group share one plate, with thin dividers between them.
pub type Groups = Vec<Vec<Item>>;

// Groups run from the left edge. The strip is the Control Strip,
// which hugs the right edge. It is empty while the strip is expanded.
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    pub groups: Groups,
    pub strip: Vec<Item>,
}

impl Layout {
    // Every plate in order: each group, then the strip.
    pub fn plates(&self) -> impl Iterator<Item = &[Item]> {
        let groups = self.groups.iter().map(Vec::as_slice);
        groups.chain(std::iter::once(self.strip.as_slice()))
    }

    // All items left to right.
    pub fn items(&self) -> impl Iterator<Item = &Item> {
        self.plates().flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(label: &str) -> Item {
        Item::Key(Key { label: label.to_string(), action: Action::ToggleStrip })
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
