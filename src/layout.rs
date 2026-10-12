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

// Keys in a group share one plate, with thin dividers between them.
pub type Groups = Vec<Vec<Key>>;

// Groups run from the left edge. The strip is the Control Strip,
// which hugs the right edge. It is empty while the strip is expanded.
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    pub groups: Groups,
    pub strip: Vec<Key>,
}

impl Layout {
    // Every plate in order: each group, then the strip.
    pub fn plates(&self) -> impl Iterator<Item = &[Key]> {
        let groups = self.groups.iter().map(Vec::as_slice);
        groups.chain(std::iter::once(self.strip.as_slice()))
    }

    // All keys left to right.
    pub fn keys(&self) -> impl Iterator<Item = &Key> {
        self.plates().flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(label: &str) -> Key {
        Key { label: label.to_string(), action: Action::ToggleStrip }
    }

    #[test]
    fn keys_run_through_the_groups_then_the_strip() {
        let layout = Layout {
            groups: vec![vec![key("a")], vec![key("b"), key("c")]],
            strip: vec![key("d")],
        };
        let labels: Vec<&str> = layout.keys().map(|k| k.label.as_str()).collect();
        assert_eq!(labels, ["a", "b", "c", "d"]);
        assert_eq!(layout.plates().count(), 3);
    }
}
