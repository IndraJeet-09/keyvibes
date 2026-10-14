//! Pack browsing logic for the desktop UI.
//!
//! Everything here is plain data in, plain data out: no GTK, no I/O, no
//! engine. The window calls [`filter`] whenever the search entry or a
//! category chip changes and rebuilds its rows from the result, and the
//! unit tests exercise the same functions the UI does.

use kv_control::PackInfo;

/// A category chip in the search bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CategoryFilter {
    /// Every installed pack.
    All,
    /// The `linear` group.
    Linear,
    /// The `tactile` group.
    Tactile,
    /// The `clicky` group.
    Clicky,
    /// The packs KeyVibes synthesizes itself, always shown separately so a
    /// user can never mistake them for field recordings.
    Demo,
}

impl CategoryFilter {
    /// Every chip, in the order the UI lays them out.
    pub const ALL: [CategoryFilter; 5] = [
        CategoryFilter::All,
        CategoryFilter::Linear,
        CategoryFilter::Tactile,
        CategoryFilter::Clicky,
        CategoryFilter::Demo,
    ];

    /// The label printed on the chip.
    pub fn label(self) -> &'static str {
        match self {
            CategoryFilter::All => "All",
            CategoryFilter::Linear => "Linear",
            CategoryFilter::Tactile => "Tactile",
            CategoryFilter::Clicky => "Clicky",
            CategoryFilter::Demo => "Demo",
        }
    }

    /// The label used in row subtitles and sort order.
    pub fn title(self) -> &'static str {
        match self {
            CategoryFilter::All => "All packs",
            CategoryFilter::Linear => "Linear",
            CategoryFilter::Tactile => "Tactile",
            CategoryFilter::Clicky => "Clicky",
            CategoryFilter::Demo => "Synthesized demo",
        }
    }
}

/// What the user is currently asking to see.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PackQuery {
    /// Free text; matched case-insensitively against several fields.
    pub text: String,
    /// The selected category chip.
    pub category: Option<CategoryFilter>,
}

impl PackQuery {
    /// Builds a query from the search entry's current text.
    pub fn new(text: impl Into<String>, category: Option<CategoryFilter>) -> Self {
        Self {
            text: text.into(),
            category,
        }
    }
}

/// The group a pack belongs to for filtering and sorting.
///
/// Synthesized packs always land in [`CategoryFilter::Demo`], whatever the
/// manifest says, so the distinction the honesty rules require is enforced
/// in one place.
pub fn group_of(pack: &PackInfo) -> CategoryFilter {
    if pack.synthesized {
        return CategoryFilter::Demo;
    }
    match pack.category.to_ascii_lowercase().as_str() {
        "linear" => CategoryFilter::Linear,
        "tactile" => CategoryFilter::Tactile,
        "clicky" => CategoryFilter::Clicky,
        _ => CategoryFilter::All,
    }
}

/// Whether `pack` satisfies `query`.
///
/// Text matching is deliberately generous - name, category, author, source
/// and description all participate - because a user searching "MIT" or
/// "kbsim" should find something rather than nothing.
pub fn matches(pack: &PackInfo, query: &PackQuery) -> bool {
    if let Some(category) = query.category {
        if category != CategoryFilter::All && group_of(pack) != category {
            return false;
        }
    }

    let needle = query.text.trim().to_ascii_lowercase();
    if needle.is_empty() {
        return true;
    }
    let haystack = [
        pack.name.as_str(),
        pack.category.as_str(),
        pack.author.as_deref().unwrap_or_default(),
        pack.license.as_deref().unwrap_or_default(),
        pack.source.as_deref().unwrap_or_default(),
        pack.description.as_deref().unwrap_or_default(),
    ]
    .join(" ")
    .to_ascii_lowercase();
    haystack.contains(&needle)
}

/// The packs matching `query`, in display order.
///
/// Ordering puts the three real switch categories first (linear, tactile,
/// clicky), then anything uncategorized, then the synthesized demos, and
/// sorts each group by name. Deterministic, so a selection survives a
/// re-filter instead of jumping around under the cursor.
pub fn filter<'a>(packs: &'a [PackInfo], query: &PackQuery) -> Vec<&'a PackInfo> {
    let mut selected: Vec<&PackInfo> = packs.iter().filter(|pack| matches(pack, query)).collect();
    selected.sort_by(|a, b| {
        group_rank(a).cmp(&group_rank(b)).then_with(|| {
            a.name
                .to_ascii_lowercase()
                .cmp(&b.name.to_ascii_lowercase())
        })
    });
    selected
}

fn group_rank(pack: &PackInfo) -> u8 {
    match group_of(pack) {
        CategoryFilter::Linear => 0,
        CategoryFilter::Tactile => 1,
        CategoryFilter::Clicky => 2,
        CategoryFilter::All => 3,
        CategoryFilter::Demo => 4,
    }
}

/// A short, honest coverage string: `104/104 keys` or `96/104 keys`.
pub fn coverage_label(pack: &PackInfo) -> String {
    format!("{}/104 keys", pack.coverage)
}

/// One line of provenance for a row subtitle.
///
/// Returns an empty string when the manifest says nothing, so the UI can
/// hide the label instead of printing a dangling separator.
pub fn provenance_line(pack: &PackInfo) -> String {
    let author = pack.author.as_deref().unwrap_or("").trim();
    let license = pack.license.as_deref().unwrap_or("").trim();
    if author.is_empty() {
        return license.to_string();
    }
    // Upstream packs ship `author = "<name> · <licence>"`, so appending the
    // licence again would print it twice.
    if license.is_empty() || author_states_license(author, license) {
        return author.to_string();
    }
    format!("{author} · {license}")
}

/// Whether `author` already names `license` among its `·`-separated parts.
fn author_states_license(author: &str, license: &str) -> bool {
    author.split('\u{b7}').map(str::trim).any(|part| {
        part.eq_ignore_ascii_case(license)
            || part.eq_ignore_ascii_case(&format!("{license} License"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack(name: &str, category: &str, synthesized: bool) -> PackInfo {
        PackInfo {
            name: name.to_string(),
            category: category.to_string(),
            path: format!("/packs/{name}.kvpack"),
            keys: 104,
            release_keys: 0,
            clips: 104,
            sample_rate: 48_000,
            coverage: 104,
            has_release: false,
            author: Some("kbsim".to_string()),
            license: Some("MIT".to_string()),
            source: Some("rustyvibes-v2".to_string()),
            description: Some("a field-recorded linear switch pack".to_string()),
            synthesized,
        }
    }

    fn shelf() -> Vec<PackInfo> {
        vec![
            pack("Topre", "clicky", false),
            pack("Holy Panda", "tactile", false),
            pack("Alpaca", "linear", false),
            pack("Default", "", true),
            pack("Linear", "", true),
            pack("Kailh Box Navy", "clicky", false),
        ]
    }

    #[test]
    fn all_shows_everything() {
        let packs = shelf();
        let query = PackQuery::new("", None);
        assert_eq!(filter(&packs, &query).len(), 6);
    }

    #[test]
    fn categories_split_the_upstream_packs() {
        let packs = shelf();
        let linear = filter(&packs, &PackQuery::new("", Some(CategoryFilter::Linear)));
        assert_eq!(linear.len(), 1);
        assert_eq!(linear[0].name, "Alpaca");

        let tactile = filter(&packs, &PackQuery::new("", Some(CategoryFilter::Tactile)));
        assert_eq!(tactile.len(), 1);

        let clicky = filter(&packs, &PackQuery::new("", Some(CategoryFilter::Clicky)));
        assert_eq!(clicky.len(), 2);
    }

    #[test]
    fn demo_group_is_only_synthesized_packs() {
        let packs = shelf();
        let demo = filter(&packs, &PackQuery::new("", Some(CategoryFilter::Demo)));
        let names: Vec<&str> = demo.iter().map(|pack| pack.name.as_str()).collect();
        assert_eq!(names, vec!["Default", "Linear"]);
        assert!(demo.iter().all(|pack| pack.synthesized));
    }

    #[test]
    fn a_synthesized_pack_never_lands_in_a_switch_group() {
        // The demo manifests carry no `category`; even if one did, the
        // synthesized flag wins so the label stays honest.
        let mut pack = pack("Sneaky", "linear", true);
        assert_eq!(group_of(&pack), CategoryFilter::Demo);
        pack.synthesized = false;
        assert_eq!(group_of(&pack), CategoryFilter::Linear);
    }

    #[test]
    fn text_matches_name_author_license_and_description() {
        let packs = shelf();
        for needle in ["alpaca", "KBSIM", "mit", "field", "rustyvibes"] {
            let query = PackQuery::new(needle, None);
            assert!(
                !filter(&packs, &query).is_empty(),
                "expected a hit for {needle:?}"
            );
        }
        let query = PackQuery::new("zzz-nothing", None);
        assert!(filter(&packs, &query).is_empty());
    }

    #[test]
    fn text_and_category_combine() {
        let packs = shelf();
        let query = PackQuery::new("alpaca", Some(CategoryFilter::Clicky));
        assert!(filter(&packs, &query).is_empty());

        let query = PackQuery::new("alpaca", Some(CategoryFilter::Linear));
        assert_eq!(filter(&packs, &query).len(), 1);
    }

    #[test]
    fn whitespace_only_search_is_not_a_filter() {
        let packs = shelf();
        let query = PackQuery::new("   ", Some(CategoryFilter::All));
        assert_eq!(filter(&packs, &query).len(), 6);
    }

    #[test]
    fn ordering_is_groups_then_name() {
        let packs = shelf();
        let query = PackQuery::new("", None);
        let names: Vec<&str> = filter(&packs, &query)
            .iter()
            .map(|pack| pack.name.as_str())
            .collect();
        assert_eq!(
            names,
            vec![
                "Alpaca",
                "Holy Panda",
                "Kailh Box Navy",
                "Topre",
                "Default",
                "Linear",
            ]
        );
    }

    #[test]
    fn coverage_and_provenance_labels() {
        let mut pack = pack("Alpaca", "linear", false);
        pack.coverage = 96;
        assert_eq!(coverage_label(&pack), "96/104 keys");
        assert_eq!(provenance_line(&pack), "kbsim · MIT");

        // The shape every converted upstream pack actually has: the licence
        // is already inside `author`, so it must not be repeated.
        let upstream = PackInfo {
            author: Some("kbsim by Thomas Lai \u{b7} MIT License".to_string()),
            license: Some("MIT".to_string()),
            ..pack.clone()
        };
        assert_eq!(
            provenance_line(&upstream),
            "kbsim by Thomas Lai \u{b7} MIT License"
        );

        let author_only = PackInfo {
            license: None,
            ..pack.clone()
        };
        assert_eq!(provenance_line(&author_only), "kbsim");

        let bare = PackInfo {
            author: None,
            license: None,
            ..pack
        };
        assert_eq!(provenance_line(&bare), "");
        assert_eq!(group_of(&bare), CategoryFilter::Linear);
    }

    #[test]
    fn release_availability_is_read_from_the_pack() {
        let mut pack = pack("With release", "linear", false);
        pack.has_release = true;
        pack.release_keys = 104;
        assert!(pack.has_release);
        assert_eq!(pack.release_keys, 104);
    }
}
