//! Merging the two market sources and searching the result.
//!
//! The signed index wins over topic discovery for the same plugin id: a
//! reviewed entry carries the category, trust and hashes the index published,
//! while the discovered one only has what the repository itself said.

use std::collections::BTreeSet;

use vibex_core::{
    PluginCategory, PluginMarketEntry, PluginMarketSearchRequest, PluginMarketSearchResponse,
    PluginMarketSort,
};

use super::index::PluginIndexEntry;

/// The default page size a search returns.
pub const DEFAULT_SEARCH_LIMIT: u32 = 30;
/// The largest page a search may return.
pub const MAX_SEARCH_LIMIT: u32 = 200;

/// Merges the index and topic entries, de-duplicated by plugin id.
pub fn merge_entries(
    index_entries: Vec<PluginIndexEntry>,
    github_entries: Vec<PluginIndexEntry>,
) -> Vec<PluginIndexEntry> {
    let mut merged = Vec::with_capacity(index_entries.len() + github_entries.len());
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for entry in index_entries {
        if seen.insert(entry.entry.id.to_ascii_lowercase()) {
            merged.push(entry);
        }
    }
    for entry in github_entries {
        if seen.insert(entry.entry.id.to_ascii_lowercase()) {
            merged.push(entry);
        }
    }
    merged
}

/// Searches, filters and pages a merged catalog.
pub fn search_entries(
    entries: &[PluginIndexEntry],
    request: &PluginMarketSearchRequest,
) -> PluginMarketSearchResponse {
    let query = request
        .query
        .as_deref()
        .map(str::trim)
        .filter(|query| !query.is_empty())
        .map(str::to_lowercase);
    let mut matches: Vec<&PluginIndexEntry> = entries
        .iter()
        .filter(|entry| matches_category(&entry.entry, request.category))
        .filter(|entry| match &query {
            Some(query) => matches_query(&entry.entry, query),
            None => true,
        })
        .collect();

    sort_entries(&mut matches, request.sort);
    let total_matches = matches.len();
    let offset = request.offset.unwrap_or(0) as usize;
    let limit = request
        .limit
        .unwrap_or(DEFAULT_SEARCH_LIMIT)
        .min(MAX_SEARCH_LIMIT) as usize;
    let page: Vec<PluginMarketEntry> = matches
        .into_iter()
        .skip(offset)
        .take(limit)
        .map(|entry| entry.entry.clone())
        .collect();
    let has_more = offset + page.len() < total_matches;
    PluginMarketSearchResponse {
        entries: page,
        has_more,
        total_matches,
    }
}

fn matches_category(entry: &PluginMarketEntry, category: Option<PluginCategory>) -> bool {
    match category {
        Some(category) => entry.categories.contains(&category),
        None => true,
    }
}

fn matches_query(entry: &PluginMarketEntry, query: &str) -> bool {
    let mut haystack = String::new();
    haystack.push_str(&entry.name);
    haystack.push(' ');
    haystack.push_str(&entry.id);
    haystack.push(' ');
    haystack.push_str(&entry.publisher.id);
    haystack.push(' ');
    haystack.push_str(&entry.publisher.display_name);
    if let Some(summary) = &entry.summary {
        haystack.push(' ');
        haystack.push_str(summary);
    }
    for tag in &entry.tags {
        haystack.push(' ');
        haystack.push_str(tag);
    }
    for category in &entry.categories {
        haystack.push(' ');
        haystack.push_str(category.key());
    }
    for localized in entry.i18n.values() {
        if let Some(name) = &localized.name {
            haystack.push(' ');
            haystack.push_str(name);
        }
        if let Some(summary) = &localized.summary {
            haystack.push(' ');
            haystack.push_str(summary);
        }
    }
    haystack.to_lowercase().contains(query)
}

/// Orders entries by the requested ranking.
pub fn sort_entries(entries: &mut [&PluginIndexEntry], sort: PluginMarketSort) {
    match sort {
        PluginMarketSort::Recommended => entries.sort_by(|left, right| {
            right
                .entry
                .featured
                .cmp(&left.entry.featured)
                .then_with(|| right.entry.installs.cmp(&left.entry.installs))
                .then_with(|| {
                    left.entry
                        .name
                        .to_lowercase()
                        .cmp(&right.entry.name.to_lowercase())
                })
        }),
        PluginMarketSort::Installs => entries.sort_by(|left, right| {
            right
                .entry
                .installs
                .cmp(&left.entry.installs)
                .then_with(|| left.entry.id.cmp(&right.entry.id))
        }),
        PluginMarketSort::Updated => entries.sort_by(|left, right| {
            right
                .entry
                .updated_at
                .cmp(&left.entry.updated_at)
                .then_with(|| left.entry.id.cmp(&right.entry.id))
        }),
        PluginMarketSort::Name => entries.sort_by(|left, right| {
            left.entry
                .name
                .to_lowercase()
                .cmp(&right.entry.name.to_lowercase())
                .then_with(|| left.entry.id.cmp(&right.entry.id))
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::{PluginListingSource, PluginPublisher, PluginTrust};

    fn entry(
        id: &str,
        name: &str,
        installs: u64,
        categories: Vec<PluginCategory>,
    ) -> PluginIndexEntry {
        PluginIndexEntry {
            entry: PluginMarketEntry {
                id: id.to_string(),
                name: name.to_string(),
                kind: Default::default(),
                bundled: false,
                listing_source: PluginListingSource::Index,
                summary: Some(format!("{name} does things")),
                icon_url: None,
                publisher: PluginPublisher::from_login("acme"),
                categories,
                tags: vec!["widget".to_string()],
                latest_version: Some("1.0.0".to_string()),
                installs,
                rating: None,
                updated_at: None,
                license: Some("MIT".to_string()),
                homepage: None,
                repository: None,
                trust: PluginTrust::Community,
                permissions: Vec::new(),
                compatibility: Default::default(),
                size_bytes: None,
                featured: false,
                i18n: Default::default(),
            },
            versions: Vec::new(),
            publisher_key_id: None,
            description: None,
            readme: None,
            screenshots: Vec::new(),
            reviews: None,
        }
    }

    #[test]
    fn the_index_wins_a_duplicate_id() {
        let mut index = entry("acme.widgets", "Widgets", 5, vec![]);
        index.entry.trust = PluginTrust::Verified;
        let mut github = entry("acme.widgets", "Widgets (repo)", 0, vec![]);
        github.entry.listing_source = PluginListingSource::Github;
        let merged = merge_entries(vec![index], vec![github]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].entry.trust, PluginTrust::Verified);
        assert_eq!(merged[0].entry.name, "Widgets");
    }

    #[test]
    fn search_matches_name_publisher_tags_and_category() {
        let entries = vec![
            entry("acme.widgets", "Widgets", 10, vec![PluginCategory::Panels]),
            entry("acme.charts", "Charts", 20, vec![PluginCategory::Viewers]),
        ];
        let by_name = search_entries(
            &entries,
            &PluginMarketSearchRequest {
                query: Some("chart".to_string()),
                ..Default::default()
            },
        );
        assert_eq!(by_name.total_matches, 1);
        assert_eq!(by_name.entries[0].id, "acme.charts");

        let by_category = search_entries(
            &entries,
            &PluginMarketSearchRequest {
                category: Some(PluginCategory::Panels),
                ..Default::default()
            },
        );
        assert_eq!(by_category.total_matches, 1);

        let by_tag = search_entries(
            &entries,
            &PluginMarketSearchRequest {
                query: Some("widget".to_string()),
                ..Default::default()
            },
        );
        assert_eq!(by_tag.total_matches, 2);
    }

    #[test]
    fn installs_sort_pages_and_reports_more() {
        let entries = vec![
            entry("a.one", "One", 1, vec![]),
            entry("a.two", "Two", 5, vec![]),
            entry("a.three", "Three", 3, vec![]),
        ];
        let first = search_entries(
            &entries,
            &PluginMarketSearchRequest {
                sort: PluginMarketSort::Installs,
                limit: Some(2),
                ..Default::default()
            },
        );
        assert_eq!(first.total_matches, 3);
        assert!(first.has_more);
        assert_eq!(first.entries[0].id, "a.two");
        assert_eq!(first.entries[1].id, "a.three");

        let second = search_entries(
            &entries,
            &PluginMarketSearchRequest {
                sort: PluginMarketSort::Installs,
                limit: Some(2),
                offset: Some(2),
                ..Default::default()
            },
        );
        assert!(!second.has_more);
        assert_eq!(second.entries[0].id, "a.one");
    }
}
