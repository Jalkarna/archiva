#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OrderedMap<K, V> {
    entries: Vec<(K, V)>,
}

impl<K, V> Default for OrderedMap<K, V> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
        }
    }
}

impl<K, V> OrderedMap<K, V>
where
    K: Eq,
{
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_entries(entries: Vec<(K, V)>) -> Self {
        let mut map = Self::new();
        for (key, value) in entries {
            map.insert(key, value);
        }
        map
    }

    pub fn insert(&mut self, key: K, value: V) {
        if let Some((_, existing)) = self
            .entries
            .iter_mut()
            .find(|(existing, _)| existing == &key)
        {
            *existing = value;
            return;
        }
        self.entries.push((key, value));
    }

    /// Append an entry whose key the caller guarantees is not already present,
    /// skipping the linear duplicate scan `insert` performs. Using this in a
    /// per-item build loop keeps construction O(n) instead of O(n²); passing a
    /// key that already exists silently creates a shadowed duplicate, so only
    /// use it when uniqueness is established by construction.
    pub fn push_unique(&mut self, key: K, value: V) {
        self.entries.push((key, value));
    }

    pub fn get(&self, key: &K) -> Option<&V> {
        self.entries
            .iter()
            .find(|(existing, _)| existing == key)
            .map(|(_, value)| value)
    }

    pub fn get_str(&self, key: &str) -> Option<&V>
    where
        K: AsRef<str>,
    {
        self.entries
            .iter()
            .find(|(existing, _)| existing.as_ref() == key)
            .map(|(_, value)| value)
    }

    pub fn remove_str(&mut self, key: &str) -> Option<V>
    where
        K: AsRef<str>,
    {
        let index = self
            .entries
            .iter()
            .position(|(existing, _)| existing.as_ref() == key)?;
        Some(self.entries.remove(index).1)
    }

    pub fn entries(&self) -> &[(K, V)] {
        &self.entries
    }

    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.entries.iter().map(|(key, value)| (key, value))
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&K, &mut V)> {
        self.entries.iter_mut().map(|(key, value)| (&*key, value))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::OrderedMap;

    #[test]
    fn preserves_insertion_order_and_replaces_values_in_place() {
        let mut map = OrderedMap::new();
        map.insert("z".to_string(), 1);
        map.insert("a".to_string(), 2);
        map.insert("z".to_string(), 3);

        assert_eq!(map.len(), 2);
        assert_eq!(map.get_str("z"), Some(&3));
        assert_eq!(map.remove_str("z"), Some(3));
        assert_eq!(map.len(), 1);
        assert_eq!(
            map.iter()
                .map(|(key, value)| (key.as_str(), *value))
                .collect::<Vec<_>>(),
            vec![("a", 2)]
        );

        for (_, value) in map.iter_mut() {
            *value += 1;
        }
        assert_eq!(map.get_str("a"), Some(&3));
    }
}
