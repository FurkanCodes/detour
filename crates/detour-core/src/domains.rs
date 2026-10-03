//! Domain lists with suffix matching: an entry `discord.com` matches
//! `discord.com` and every subdomain such as `updates.discord.com`.

use std::borrow::Cow;
use std::collections::HashSet;

#[derive(Debug, Default, Clone)]
pub struct DomainList {
    entries: HashSet<String>,
}

impl DomainList {
    pub fn new() -> Self {
        Self::default()
    }

    /// Parses a list file: one domain per line, `#` starts a comment,
    /// blank lines and a leading `*.` are ignored.
    pub fn parse(text: &str) -> Self {
        let mut list = Self::new();
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            if !line.is_empty() {
                list.insert(line);
            }
        }
        list
    }

    pub fn insert(&mut self, domain: &str) -> bool {
        match normalize(domain) {
            Some(d) => self.entries.insert(d.into_owned()),
            None => false,
        }
    }

    pub fn remove(&mut self, domain: &str) -> bool {
        normalize(domain).is_some_and(|d| self.entries.remove(d.as_ref()))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// True if `host` or any of its parent domains is in the list.
    pub fn matches(&self, host: &str) -> bool {
        let Some(host) = normalize(host) else {
            return false;
        };
        let mut rest = host.as_ref();
        loop {
            if self.entries.contains(rest) {
                return true;
            }
            match rest.find('.') {
                Some(i) => rest = &rest[i + 1..],
                None => return false,
            }
        }
    }

    /// Entries sorted alphabetically, for display and saving.
    pub fn sorted(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self.entries.iter().map(String::as_str).collect();
        v.sort_unstable();
        v
    }

    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for d in self.sorted() {
            out.push_str(d);
            out.push('\n');
        }
        out
    }

    pub fn extend(&mut self, other: &DomainList) {
        self.entries.extend(other.entries.iter().cloned());
    }
}

/// Lowercases, strips `*.`, surrounding dots and whitespace. Rejects
/// strings that cannot be hostnames.
fn normalize(domain: &str) -> Option<Cow<'_, str>> {
    let d = domain.trim().trim_start_matches("*.").trim_matches('.');
    if d.is_empty() || d.len() > 253 {
        return None;
    }
    if !d
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
    {
        return None;
    }
    if d.split('.')
        .any(|label| label.is_empty() || label.len() > 63)
    {
        return None;
    }
    Some(if d.bytes().any(|b| b.is_ascii_uppercase()) {
        Cow::Owned(d.to_ascii_lowercase())
    } else {
        Cow::Borrowed(d)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suffix_matching() {
        let list = DomainList::parse("discord.com\n# comment\n\n*.rbxcdn.com  # roblox\n");
        assert_eq!(list.len(), 2);
        assert!(list.matches("discord.com"));
        assert!(list.matches("updates.discord.com"));
        assert!(list.matches("UPDATES.Discord.COM."));
        assert!(list.matches("t0.rbxcdn.com"));
        assert!(!list.matches("notdiscord.com"));
        assert!(!list.matches("com"));
        assert!(!list.matches(""));
    }

    #[test]
    fn rejects_garbage() {
        let mut list = DomainList::new();
        assert!(!list.insert("bad domain.com"));
        assert!(!list.insert("a..b"));
        assert!(!list.insert(""));
        assert!(!list.insert(&"a".repeat(64)));
        assert!(list.is_empty());
    }

    #[test]
    fn round_trip_text() {
        let list = DomainList::parse("b.com\na.com\nA.com\n");
        assert_eq!(list.to_text(), "a.com\nb.com\n");
    }

    #[test]
    fn shipped_lists_parse() {
        let discord = DomainList::parse(include_str!("../../../lists/discord.txt"));
        assert!(discord.matches("updates.discord.com"));
        assert!(discord.matches("gateway.discord.gg"));
        let roblox = DomainList::parse(include_str!("../../../lists/roblox.txt"));
        assert!(roblox.matches("tr.rbxcdn.com"));
    }

    #[test]
    fn remove_entry() {
        let mut list = DomainList::parse("discord.com\n");
        assert!(list.remove("Discord.com"));
        assert!(!list.matches("discord.com"));
    }
}
