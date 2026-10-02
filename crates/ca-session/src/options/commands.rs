//! Keyboard, menu and toolbar customization, stored per view kind.
//!
//! The same key names a different command in each comparison type, so every
//! entry here is held under the view it applies to. A command with no entry
//! keeps the built-in binding; an entry that lists no keystroke means the
//! command was unbound on purpose, which is why the empty list is stored rather
//! than dropped.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// What one view kind states for itself.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ViewCommandOptions {
    /// Command name to the keystrokes that run it. An entry replaces the
    /// built-in bindings of that command.
    pub shortcuts: BTreeMap<String, Vec<String>>,
    /// Commands the toolbar shows, in order. Empty keeps the built-in order.
    pub toolbar: Vec<String>,
    /// Commands the menus leave out.
    pub hidden_from_menu: Vec<String>,
    /// Toolbar items the toolbar leaves out. An item named here keeps its
    /// place in the order and is not drawn.
    pub toolbar_hidden: Vec<String>,
    /// Fields written by another build.
    #[serde(flatten)]
    pub unknown: BTreeMap<String, Value>,
}

impl ViewCommandOptions {
    /// True when nothing is stated.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.shortcuts.is_empty()
            && self.toolbar.is_empty()
            && self.hidden_from_menu.is_empty()
            && self.toolbar_hidden.is_empty()
    }
}

/// Command customization for every view kind.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CommandOptions {
    /// View name to what that view states.
    pub views: BTreeMap<String, ViewCommandOptions>,
    /// Fields written by another build.
    #[serde(flatten)]
    pub unknown: BTreeMap<String, Value>,
}

impl CommandOptions {
    /// What `view` states, where it states anything.
    #[must_use]
    pub fn view(&self, view: &str) -> Option<&ViewCommandOptions> {
        self.views.get(view)
    }

    /// What `view` states, creating an empty entry when it states nothing.
    pub fn view_mut(&mut self, view: &str) -> &mut ViewCommandOptions {
        self.views.entry(view.to_owned()).or_default()
    }

    /// The keystrokes stated for one command, where any are.
    #[must_use]
    pub fn shortcuts(&self, view: &str, command: &str) -> Option<&[String]> {
        self.view(view)
            .and_then(|entry| entry.shortcuts.get(command))
            .map(Vec::as_slice)
    }

    /// States the keystrokes of one command. An empty list unbinds it.
    pub fn set_shortcuts(&mut self, view: &str, command: &str, keys: Vec<String>) {
        self.view_mut(view)
            .shortcuts
            .insert(command.to_owned(), keys);
    }

    /// Drops whatever one command stated, so its built-in binding applies
    /// again.
    pub fn reset_command(&mut self, view: &str, command: &str) {
        if let Some(entry) = self.views.get_mut(view) {
            entry.shortcuts.remove(command);
            if entry.is_empty() {
                self.views.remove(view);
            }
        }
    }

    /// Drops every shortcut one view stated.
    pub fn reset_view(&mut self, view: &str) {
        if let Some(entry) = self.views.get_mut(view) {
            entry.shortcuts.clear();
            if entry.is_empty() {
                self.views.remove(view);
            }
        }
    }

    /// Drops every shortcut of every view.
    pub fn reset_all_shortcuts(&mut self) {
        let views: Vec<String> = self.views.keys().cloned().collect();
        for view in views {
            self.reset_view(&view);
        }
    }

    /// The toolbar order stated for `view`, where one is.
    #[must_use]
    pub fn toolbar(&self, view: &str) -> Option<&[String]> {
        self.view(view)
            .map(|entry| entry.toolbar.as_slice())
            .filter(|order| !order.is_empty())
    }

    /// States the toolbar order of one view. An empty list keeps the built-in
    /// order.
    pub fn set_toolbar(&mut self, view: &str, order: Vec<String>) {
        self.view_mut(view).toolbar = order;
        self.prune(view);
    }

    /// True when the toolbar of `view` leaves `item` out.
    #[must_use]
    pub fn is_hidden_from_toolbar(&self, view: &str, item: &str) -> bool {
        self.view(view)
            .is_some_and(|entry| entry.toolbar_hidden.iter().any(|held| held == item))
    }

    /// Shows or hides one item on the toolbar of one view.
    pub fn set_hidden_from_toolbar(&mut self, view: &str, item: &str, hidden: bool) {
        let entry = self.view_mut(view);
        entry.toolbar_hidden.retain(|held| held != item);
        if hidden {
            entry.toolbar_hidden.push(item.to_owned());
        }
        self.prune(view);
    }

    /// Drops the toolbar order and the hidden list of one view, so the
    /// built-in toolbar applies again.
    pub fn reset_toolbar(&mut self, view: &str) {
        if let Some(entry) = self.views.get_mut(view) {
            entry.toolbar.clear();
            entry.toolbar_hidden.clear();
        }
        self.prune(view);
    }

    /// True when the menus of `view` leave `command` out.
    #[must_use]
    pub fn is_hidden_from_menu(&self, view: &str, command: &str) -> bool {
        self.view(view)
            .is_some_and(|entry| entry.hidden_from_menu.iter().any(|held| held == command))
    }

    /// Shows or hides one command in the menus of one view.
    pub fn set_hidden_from_menu(&mut self, view: &str, command: &str, hidden: bool) {
        let entry = self.view_mut(view);
        entry.hidden_from_menu.retain(|held| held != command);
        if hidden {
            entry.hidden_from_menu.push(command.to_owned());
        }
        self.prune(view);
    }

    /// Drops a view entry that states nothing.
    fn prune(&mut self, view: &str) {
        if self
            .views
            .get(view)
            .is_some_and(ViewCommandOptions::is_empty)
        {
            self.views.remove(view);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::CommandOptions;

    #[test]
    fn a_command_with_no_entry_states_nothing() {
        let options = CommandOptions::default();
        assert_eq!(options.shortcuts("text", "FindNext"), None);
    }

    #[test]
    fn an_unbound_command_states_an_empty_list_rather_than_nothing() {
        let mut options = CommandOptions::default();
        options.set_shortcuts("text", "FindNext", Vec::new());
        assert_eq!(options.shortcuts("text", "FindNext"), Some(&[][..]));
        options.reset_command("text", "FindNext");
        assert_eq!(options.shortcuts("text", "FindNext"), None);
        assert!(options.views.is_empty());
    }

    #[test]
    fn resetting_one_view_leaves_the_other_alone() {
        let mut options = CommandOptions::default();
        options.set_shortcuts("text", "FindNext", vec!["F4".to_owned()]);
        options.set_shortcuts("folder", "FindNext", vec!["F8".to_owned()]);
        options.reset_view("text");
        assert_eq!(options.shortcuts("text", "FindNext"), None);
        assert_eq!(
            options.shortcuts("folder", "FindNext"),
            Some(&["F8".to_owned()][..])
        );
        options.reset_all_shortcuts();
        assert!(options.views.is_empty());
    }

    #[test]
    fn the_toolbar_order_and_the_hidden_list_are_kept_apart() {
        let mut options = CommandOptions::default();
        options.set_toolbar("text", vec!["Find".to_owned(), "Reload".to_owned()]);
        options.set_hidden_from_menu("text", "About", true);
        assert_eq!(options.toolbar("text").unwrap().len(), 2);
        assert!(options.is_hidden_from_menu("text", "About"));
        options.set_hidden_from_menu("text", "About", false);
        assert!(!options.is_hidden_from_menu("text", "About"));
        options.set_toolbar("text", Vec::new());
        assert_eq!(options.toolbar("text"), None);
        assert!(options.views.is_empty());
    }
}
