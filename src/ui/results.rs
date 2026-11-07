use super::{CategoryFilter, Launcher, ResultRow};
use crate::{
    icon_service::{IconService, DISPLAY_ICON_SIZE},
    result::{IconSpec, ResultCategory, ResultPresentation, SearchResult},
};
use slint::{Model, ModelRc};
use std::{
    collections::HashSet,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};

#[derive(Clone)]
pub(super) struct ResultRowPayload {
    id: String,
    title: String,
    location: String,
    location_kind: String,
    icon: Option<crate::platform::IconData>,
    icon_kind: String,
    favorite: bool,
    shortcut_prefix: String,
    shortcut_prefix_width: i32,
    presentation: String,
    category_kind: String,
    pub(super) is_fallback: bool,
    section_kind: String,
    section_count: i32,
    section_more_key: String,
    headers_before: i32,
    is_best_match: bool,
    /// Set by SearchCore's evidence check. Independent of fallback/command
    /// eligibility, which [`ResultFilterState::best_match_index`] also applies.
    pub(super) best_match_eligible: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum PresentationMode {
    RootSearch,
    RootEmpty,
    #[default]
    Engine,
}

struct PresentedRow {
    index: usize,
    section: &'static str,
    count: usize,
    more: &'static str,
    best: bool,
    starts_section: bool,
}

#[derive(Default)]
pub(super) struct ResultFilterState {
    pub(super) query_id: u64,
    pub(super) all_items: Arc<Vec<SearchResult>>,
    pub(super) all_rows: Vec<ResultRowPayload>,
    pub(super) selected: String,
    pub(super) mode: PresentationMode,
}

impl ResultFilterState {
    pub(super) fn replace(
        &mut self,
        query_id: u64,
        items: Arc<Vec<SearchResult>>,
        rows: Vec<ResultRowPayload>,
        mode: Option<PresentationMode>,
    ) {
        self.query_id = query_id;
        self.all_items = items;
        self.all_rows = rows;
        if let Some(mode) = mode {
            self.mode = mode;
        }
        if self.selected != "all"
            && !self
                .categories()
                .iter()
                .any(|(key, _)| *key == self.selected)
        {
            self.selected = "all".into();
        }
    }

    pub(super) fn categories(&self) -> Vec<(&'static str, usize)> {
        const ORDER: [&str; 11] = [
            "application",
            "game",
            "website",
            "calculator",
            "control_panel",
            "settings",
            "document",
            "bookmark",
            "window",
            "command",
            "other",
        ];
        let mut counts = [0_usize; 11];
        for item in self.all_items.iter() {
            let key = effective_category_key(item);
            if let Some(index) = ORDER.iter().position(|candidate| *candidate == key) {
                counts[index] += 1;
            }
        }
        ORDER
            .into_iter()
            .zip(counts)
            .filter(|(_, count)| *count > 0)
            .collect()
    }

    pub(super) fn filters(&self) -> Vec<CategoryFilter> {
        let categories = self.categories();
        let best = self.best_match_index();
        let single_truncated = self.mode == PresentationMode::RootSearch
            && categories.len() == 1
            && (self.selected != "all"
                || categories[0]
                    .1
                    .saturating_sub(usize::from(best.is_some_and(|i| {
                        effective_category_key(&self.all_items[i]) == categories[0].0
                    })))
                    > 3);
        if categories.len() < 2 && !single_truncated {
            return Vec::new();
        }
        std::iter::once(CategoryFilter {
            key: "all".into(),
            count: self.all_items.len() as i32,
        })
        .chain(categories.into_iter().map(|(key, count)| CategoryFilter {
            key: key.into(),
            count: count as i32,
        }))
        .collect()
    }

    pub(super) fn visible(&self) -> (Arc<Vec<SearchResult>>, Vec<ResultRowPayload>) {
        let indices = self.presentation_indices();
        let mut items = Vec::with_capacity(indices.len());
        let mut rows = Vec::with_capacity(indices.len());
        let mut headers = 0usize;
        for presented in indices {
            let mut row = self.all_rows[presented.index].clone();
            row.section_kind = if presented.starts_section {
                presented.section.into()
            } else {
                String::new()
            };
            row.section_count = if presented.starts_section {
                presented.count as i32
            } else {
                0
            };
            row.section_more_key = if presented.starts_section {
                presented.more.into()
            } else {
                String::new()
            };
            row.headers_before = headers as i32;
            row.is_best_match = presented.best;
            if presented.starts_section {
                headers += 1;
            }
            items.push(self.all_items[presented.index].clone());
            rows.push(row);
        }
        (Arc::new(items), rows)
    }

    pub(super) fn first_visible_item(&self) -> Option<&SearchResult> {
        self.presentation_indices()
            .first()
            .map(|row| &self.all_items[row.index])
    }

    fn best_match_index(&self) -> Option<usize> {
        self.all_items
            .iter()
            .zip(&self.all_rows)
            .position(|(item, row)| {
                row.best_match_eligible
                    && !row.is_fallback
                    && effective_category_key(item) != "command"
            })
    }

    fn presentation_indices(&self) -> Vec<PresentedRow> {
        let selected = self.selected.is_empty() || self.selected == "all";
        if self.mode == PresentationMode::Engine {
            return self
                .all_items
                .iter()
                .zip(&self.all_rows)
                .enumerate()
                .filter(|(_, (item, _))| selected || effective_category_key(item) == self.selected)
                .map(|(index, _)| PresentedRow {
                    index,
                    section: "",
                    count: 0,
                    more: "",
                    best: false,
                    starts_section: false,
                })
                .collect();
        }
        if !selected {
            let matches = self
                .all_items
                .iter()
                .enumerate()
                .filter(|(_, item)| effective_category_key(item) == self.selected)
                .map(|(i, _)| i)
                .collect::<Vec<_>>();
            let n = matches.len();
            return matches
                .into_iter()
                .enumerate()
                .map(|(j, index)| PresentedRow {
                    index,
                    section: self.selected_key(),
                    count: n,
                    more: "",
                    best: false,
                    starts_section: j == 0,
                })
                .collect();
        }
        let best = if self.mode == PresentationMode::RootSearch {
            self.best_match_index()
        } else {
            None
        };
        let mut groups: Vec<(&'static str, Vec<usize>)> = Vec::new();
        let mut fallback = Vec::new();
        let mut empty_groups: [Vec<usize>; 3] = [Vec::new(), Vec::new(), Vec::new()];
        for (index, (item, row)) in self.all_items.iter().zip(&self.all_rows).enumerate() {
            if Some(index) == best {
                continue;
            }
            if self.mode == PresentationMode::RootEmpty {
                let group = if effective_category_key(item) == "command" {
                    1
                } else if row.favorite {
                    0
                } else {
                    2
                };
                empty_groups[group].push(index);
            } else if row.is_fallback && effective_category_key(item) != "command" {
                fallback.push(index);
            } else {
                let key = effective_category_key(item);
                if let Some((_, indices)) = groups.iter_mut().find(|(group, _)| *group == key) {
                    indices.push(index);
                } else {
                    groups.push((key, vec![index]));
                }
            }
        }
        if self.mode == PresentationMode::RootEmpty {
            for (key, indices) in ["favorites", "command", "suggestions"]
                .into_iter()
                .zip(empty_groups)
            {
                if !indices.is_empty() {
                    groups.push((key, indices));
                }
            }
        }
        let mut output = Vec::new();
        if let Some(index) = best {
            output.push(PresentedRow {
                index,
                section: "best_match",
                count: 1,
                more: "",
                best: true,
                starts_section: true,
            });
        }
        for (key, indices) in groups {
            let count = indices.len();
            let cap = self.mode == PresentationMode::RootSearch;
            let more = if cap && count > 3 { key } else { "" };
            let take = if cap { count.min(3) } else { count };
            for (n, index) in indices.into_iter().take(take).enumerate() {
                output.push(PresentedRow {
                    index,
                    section: key,
                    count,
                    more: if n == 0 { more } else { "" },
                    best: false,
                    starts_section: n == 0,
                });
            }
        }
        if !fallback.is_empty() {
            let count = fallback.len();
            for (n, index) in fallback.into_iter().enumerate() {
                output.push(PresentedRow {
                    index,
                    section: "fallbacks",
                    count,
                    more: "",
                    best: false,
                    starts_section: n == 0,
                });
            }
        }
        output
    }
    fn selected_key(&self) -> &'static str {
        self.all_items
            .iter()
            .find(|item| effective_category_key(item) == self.selected)
            .map(|item| effective_category_key(item))
            .unwrap_or("other")
    }
}

fn effective_category_key(item: &SearchResult) -> &'static str {
    category_key(item.effective_category())
}

fn category_key(category: ResultCategory) -> &'static str {
    match category {
        ResultCategory::Application => "application",
        ResultCategory::Game => "game",
        ResultCategory::Website => "website",
        ResultCategory::Calculator => "calculator",
        ResultCategory::ControlPanel => "control_panel",
        ResultCategory::Settings => "settings",
        ResultCategory::Document => "document",
        ResultCategory::Bookmark => "bookmark",
        ResultCategory::Window => "window",
        ResultCategory::Command => "command",
        ResultCategory::Other => "other",
        ResultCategory::Unclassified => "other",
    }
}

impl ResultRowPayload {
    pub(super) fn from(
        item: &SearchResult,
        show_advanced_details: bool,
        shortcut_hint: Option<&str>,
        favorite: bool,
        is_fallback: bool,
    ) -> Self {
        let location = display_location(item, show_advanced_details);
        let shortcut_prefix = shortcut_hint.unwrap_or_default().trim_end();
        let uses_application_fallback = !show_advanced_details
            && item.category == ResultCategory::Application
            && crate::providers::app::preferred_launch_entry(item).is_some();
        Self {
            id: item.id.clone(),
            title: item.title.clone(),
            location,
            location_kind: if uses_application_fallback {
                "application"
            } else if item.category == ResultCategory::ControlPanel {
                "control-panel"
            } else {
                ""
            }
            .into(),
            icon: None,
            icon_kind: static_icon_kind(item.icon.as_ref()).into(),
            favorite,
            shortcut_prefix_width: shortcut_prefix_width(shortcut_prefix),
            shortcut_prefix: shortcut_prefix.into(),
            presentation: presentation_name(item.recommended_presentation()).into(),
            category_kind: match effective_category_key(item) {
                "window" => "window",
                "bookmark" => "bookmark",
                _ => "",
            }
            .into(),
            is_fallback,
            section_kind: String::new(),
            section_count: 0,
            section_more_key: String::new(),
            headers_before: 0,
            is_best_match: false,
            // Direct construction (including unit tests) keeps the historical
            // assumption that the first ordinary row may headline the list.
            // Search presentation overrides this with lexical evidence.
            best_match_eligible: true,
        }
    }
}

fn shortcut_prefix_width(prefix: &str) -> i32 {
    if prefix.is_empty() {
        return 44;
    }
    (16 + prefix.chars().count() as i32 * 7).clamp(44, 120)
}

/// Compare only the fields that determine the visible result row. Loaded
/// icons are intentionally ignored because they arrive asynchronously and can
/// be updated in-place without rebuilding the result model.
pub(super) fn matches_model(model: &ModelRc<ResultRow>, payloads: &[ResultRowPayload]) -> bool {
    if model.row_count() != payloads.len() {
        return false;
    }
    payloads.iter().enumerate().all(|(index, payload)| {
        let Some(row) = model.row_data(index) else {
            return false;
        };
        row.id.as_str() == payload.id
            && row.title.as_str() == payload.title
            && row.location.as_str() == payload.location
            && row.location_kind.as_str() == payload.location_kind
            && row.icon_kind.as_str() == payload.icon_kind
            && row.favorite == payload.favorite
            && row.shortcut_prefix.as_str() == payload.shortcut_prefix
            && row.shortcut_prefix_width == payload.shortcut_prefix_width
            && row.presentation.as_str() == payload.presentation
            && row.category_kind.as_str() == payload.category_kind
            && row.section_kind.as_str() == payload.section_kind
            && row.section_count == payload.section_count
            && row.section_more_key.as_str() == payload.section_more_key
            && row.headers_before == payload.headers_before
            && row.is_best_match == payload.is_best_match
    })
}

fn presentation_name(presentation: ResultPresentation) -> &'static str {
    presentation.as_str()
}

pub(super) fn static_icon_kind(icon: Option<&IconSpec>) -> &'static str {
    match icon {
        Some(IconSpec::Calculator) => "calculator",
        Some(IconSpec::Named(name)) => match name.as_str() {
            "book" | "wowie" => "book",
            "bookmark" => "bookmark",
            "window" => "window",
            "checklist" => "checklist",
            "folder-open" => "folder-open",
            "shield-account" => "shield-account",
            "content-copy" => "content-copy",
            "star" => "star",
            "weather" => "weather",
            "calculator" => "calculator",
            _ => "",
        },
        _ => "",
    }
}

fn display_location(item: &SearchResult, show_advanced_details: bool) -> String {
    if item.category == ResultCategory::Bookmark {
        return if show_advanced_details {
            item.launch_target.to_string_lossy().into_owned()
        } else {
            item.location.clone()
        };
    }
    if item.category == ResultCategory::Settings {
        return if show_advanced_details {
            item.launch_target.to_string_lossy().into_owned()
        } else {
            item.location.clone()
        };
    }
    if !show_advanced_details && item.category == ResultCategory::ControlPanel {
        return String::new();
    }
    let Some(entry) = crate::providers::app::preferred_launch_entry(item) else {
        return item.location.clone();
    };
    if !show_advanced_details && item.category == ResultCategory::Application {
        return item.description.clone().unwrap_or_else(|| {
            if crate::platform::windows_rules::is_shell_namespace(&entry.target) {
                String::new()
            } else {
                crate::platform::application_description(&entry.target).unwrap_or_default()
            }
        });
    }
    let target = entry.target.to_string_lossy();
    let is_shell_or_url =
        crate::platform::windows_rules::is_shell_namespace(&entry.target) || target.contains("://");
    let base = if show_advanced_details || is_shell_or_url {
        target.into_owned()
    } else {
        entry
            .target
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_owned)
            .unwrap_or_else(|| item.location.clone())
    };
    if entry.arguments.trim().is_empty() {
        base
    } else {
        format!("{base} {}", entry.arguments.trim())
    }
}

pub(super) type IconItemState = Arc<Mutex<(u64, Arc<Vec<SearchResult>>)>>;

pub(super) fn request_icons(
    start: i32,
    end: i32,
    icon_items: &IconItemState,
    icon_requests: &Arc<Mutex<(u64, HashSet<usize>)>>,
    icon_service: &IconService,
    weak: &slint::Weak<Launcher>,
    query_generation: &Arc<AtomicU64>,
) {
    let (query_id, items) = icon_items
        .lock()
        .map(|state| (state.0, Arc::clone(&state.1)))
        .unwrap_or_else(|_| (0, Arc::new(Vec::new())));
    if query_id == 0 || query_generation.load(Ordering::Acquire) != query_id {
        return;
    }
    let start = start.max(0) as usize;
    let end = (end.max(start as i32) as usize).min(items.len());
    if start >= end {
        return;
    }
    let mut pending = Vec::new();
    if let Ok(mut state) = icon_requests.lock() {
        if state.0 != query_id {
            *state = (query_id, HashSet::new());
        }
        for index in start..end {
            if state.1.insert(index) && static_icon_kind(items[index].icon.as_ref()).is_empty() {
                pending.push((index, items[index].clone()));
            }
        }
    }
    if pending.is_empty() {
        return;
    }
    let icon_service = icon_service.clone();
    let weak = weak.clone();
    let query_generation = Arc::clone(query_generation);
    let cancellation = crate::utils::background::CancellationToken::new();
    let task_cancellation = cancellation.clone();
    if !crate::utils::background::spawn_cancellable(task_cancellation, move || {
        let mut resolved = Vec::with_capacity(pending.len());
        for (index, item) in pending {
            if query_generation.load(Ordering::Acquire) != query_id {
                return;
            }
            let Some(icon) = icon_service.resolve_at(&item, DISPLAY_ICON_SIZE) else {
                continue;
            };
            resolved.push((index, item.id, icon));
        }
        if resolved.is_empty() {
            return;
        }
        let _ = slint::invoke_from_event_loop(move || {
            if query_generation.load(Ordering::Acquire) != query_id {
                return;
            }
            let Some(window) = weak.upgrade() else {
                return;
            };
            let model = window.get_results();
            for (index, item_id, icon) in resolved {
                let Some(mut row) = model.row_data(index) else {
                    continue;
                };
                if row.id != item_id {
                    continue;
                }
                row.icon = icon_to_image(icon.as_ref());
                model.set_row_data(index, row);
            }
        });
    }) {
        cancellation.cancel();
    }
}

pub(super) fn icon_to_image(data: &crate::platform::IconData) -> slint::Image {
    let buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
        &data.rgba,
        data.width,
        data.height,
    );
    slint::Image::from_rgba8(buffer)
}

impl From<ResultRowPayload> for ResultRow {
    fn from(payload: ResultRowPayload) -> Self {
        let icon = payload.icon.as_ref().map(icon_to_image).unwrap_or_default();
        Self {
            id: payload.id.into(),
            title: payload.title.into(),
            location: payload.location.into(),
            location_kind: payload.location_kind.into(),
            icon,
            icon_kind: payload.icon_kind.into(),
            favorite: payload.favorite,
            shortcut_prefix: payload.shortcut_prefix.into(),
            shortcut_prefix_width: payload.shortcut_prefix_width,
            presentation: payload.presentation.into(),
            category_kind: payload.category_kind.into(),
            section_kind: payload.section_kind.into(),
            section_count: payload.section_count,
            section_more_key: payload.section_more_key.into(),
            headers_before: payload.headers_before,
            is_best_match: payload.is_best_match,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{platform::SourceId, result::ResultVisibility};
    use std::path::PathBuf;

    fn item(id: &str, category: ResultCategory) -> SearchResult {
        SearchResult {
            id: id.into(),
            identity: id.into(),
            title: id.into(),
            category,
            metadata: Default::default(),
            aliases: Vec::new(),
            location: String::new(),
            launch_target: Default::default(),
            launch_arguments: String::new(),
            launch_working_directory: None,
            source: SourceId::Shortcut,
            sources: Vec::new(),
            launch_entries: Vec::new(),
            actions: Vec::new(),
            detail: None,
            description: None,
            app_user_model_id: None,
            package_family_name: None,
            package_metadata: None,
            icon: None,
            ranking_bias: 0,
            allow_favorite: true,
            visibility: ResultVisibility::Always,
        }
    }

    #[test]
    fn category_filter_only_appears_for_mixed_results_and_preserves_order() {
        let items = Arc::new(vec![
            item("app-1", ResultCategory::Application),
            item("web", ResultCategory::Website),
            item("app-2", ResultCategory::Application),
        ]);
        let rows = items
            .iter()
            .map(|item| ResultRowPayload::from(item, false, None, false, false))
            .collect();
        let mut filter = ResultFilterState::default();
        filter.replace(1, items, rows, None);
        assert_eq!(
            filter
                .filters()
                .iter()
                .map(|option| option.key.as_str())
                .collect::<Vec<_>>(),
            ["all", "application", "website"]
        );
        filter.selected = "application".into();
        let (visible, rows) = filter.visible();
        assert_eq!(
            visible
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["app-1", "app-2"]
        );
        assert_eq!(
            rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
            ["app-1", "app-2"]
        );

        let only_web = Arc::new(vec![item("web-2", ResultCategory::Website)]);
        filter.replace(
            2,
            Arc::clone(&only_web),
            only_web
                .iter()
                .map(|item| ResultRowPayload::from(item, false, None, false, false))
                .collect(),
            None,
        );
        assert!(filter.filters().is_empty());
        assert_eq!(filter.selected, "all");
    }

    #[test]
    fn session_actions_define_exclusive_command_in_every_mode() {
        let mut entry = item("arbitrary-id", ResultCategory::Bookmark);
        entry
            .actions
            .push(crate::result::Action::EnterSearchEngine {
                prefix: "x ".into(),
            });
        let mut other_entry = item("fav-tool", ResultCategory::Other);
        other_entry.actions = entry.actions.clone();
        for mode in [
            PresentationMode::RootSearch,
            PresentationMode::RootEmpty,
            PresentationMode::Engine,
        ] {
            let mut filter = root_filter(
                vec![
                    entry.clone(),
                    item("normal-other", ResultCategory::Other),
                    other_entry.clone(),
                    item("session-bookmark", ResultCategory::Bookmark),
                ],
                mode,
                &["arbitrary-id"],
            );
            assert_eq!(
                filter.categories(),
                [("bookmark", 1), ("command", 2), ("other", 1)]
            );
            assert!(filter.all_rows[0].category_kind.is_empty());
            let (_, rows) = filter.visible();
            if mode != PresentationMode::Engine {
                assert!(rows.iter().any(|row| row.section_kind == "command"));
            }
            filter.selected = "command".into();
            let (shown, rows) = filter.visible();
            assert_eq!(
                shown
                    .iter()
                    .map(|item| item.id.as_str())
                    .collect::<Vec<_>>(),
                ["arbitrary-id", "fav-tool"]
            );
            if mode != PresentationMode::Engine {
                assert_eq!(rows[0].section_kind, "command");
            }
            filter.selected = "other".into();
            assert_eq!(filter.visible().0[0].id, "normal-other");
            filter.selected = "bookmark".into();
            assert_eq!(filter.visible().0[0].id, "session-bookmark");
        }
    }

    #[test]
    fn every_session_entry_is_command_but_ordinary_launches_keep_their_category() {
        let mut bookmark_entry = item("bookmarks:entry", ResultCategory::Bookmark);
        bookmark_entry
            .actions
            .push(crate::result::Action::EnterSearchEngine {
                prefix: "bm ".into(),
            });
        let mut window_entry = item("windows:entry", ResultCategory::Window);
        window_entry
            .actions
            .push(crate::result::Action::EnterSearchEngine {
                prefix: "< ".into(),
            });
        let mut plugin_entry = item("plugin:demo:menu", ResultCategory::Application);
        plugin_entry
            .actions
            .push(crate::result::Action::EnterSearchEngine {
                prefix: "demo ".into(),
            });
        for entry in [&bookmark_entry, &window_entry, &plugin_entry] {
            assert!(entry.is_session_entry());
            assert_eq!(entry.effective_category(), ResultCategory::Command);
            assert_eq!(effective_category_key(entry), "command");
        }

        // A launchable command-line or application result is not a session
        // entry merely because of its title or category.
        let mut launchable_command = item("shortcut:active", ResultCategory::Command);
        launchable_command.title = "Execute Command \"ipconfig\"".into();
        launchable_command
            .actions
            .push(crate::result::Action::Launch(crate::result::LaunchEntry {
                source: SourceId::Shortcut,
                path: std::path::PathBuf::new(),
                target: std::path::PathBuf::from("pwsh.exe"),
                arguments: "-Command ipconfig".into(),
                working_directory: None,
                icon: None,
            }));
        assert!(!launchable_command.is_session_entry());
        assert_eq!(effective_category_key(&launchable_command), "command");

        let application = item("application", ResultCategory::Application);
        assert_eq!(effective_category_key(&application), "application");

        // An explicitly categorized command result remains a command even
        // without a session action.
        assert_eq!(
            effective_category_key(&item("command-result", ResultCategory::Command)),
            "command"
        );
    }

    #[test]
    fn shortcut_launches_and_legacy_favorites_are_command_filterable() {
        let mut command = item("shortcut:open-url", ResultCategory::Command);
        command
            .actions
            .push(crate::result::Action::Launch(crate::result::LaunchEntry {
                source: SourceId::Shortcut,
                path: PathBuf::new(),
                target: PathBuf::from("https://example.test"),
                arguments: String::new(),
                working_directory: None,
                icon: None,
            }));

        // Persisted favorites may contain the pre-migration category. The
        // source and primary action identify these without inspecting names
        // or target strings.
        let mut old_favorite = item("shortcut:legacy", ResultCategory::Other);
        old_favorite.source = SourceId::Shortcut;
        old_favorite
            .actions
            .push(crate::result::Action::Launch(crate::result::LaunchEntry {
                source: SourceId::Shortcut,
                path: PathBuf::new(),
                target: PathBuf::from("custom-target"),
                arguments: "raw argument".into(),
                working_directory: None,
                icon: None,
            }));

        let mut application = item("app", ResultCategory::Application);
        application
            .actions
            .push(crate::result::Action::Launch(crate::result::LaunchEntry {
                source: SourceId::AppsFolder,
                path: PathBuf::new(),
                target: PathBuf::from("app.exe"),
                arguments: String::new(),
                working_directory: None,
                icon: None,
            }));
        let website = item("website", ResultCategory::Website);

        let filter = root_filter(
            vec![command, old_favorite, application, website],
            PresentationMode::RootSearch,
            &[],
        );
        assert_eq!(
            filter.categories(),
            [("application", 1), ("website", 1), ("command", 2)]
        );
        let command_ids = {
            let mut filter = filter;
            filter.selected = "command".into();
            filter
                .visible()
                .0
                .iter()
                .map(|result| result.id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(command_ids, ["shortcut:open-url", "shortcut:legacy"]);
    }

    #[test]
    fn results_inside_a_session_keep_their_provider_categories() {
        let items = Arc::new(vec![
            item("window-result", ResultCategory::Window),
            item("bookmark-result", ResultCategory::Bookmark),
            item("website-result", ResultCategory::Website),
        ]);
        let rows = items
            .iter()
            .map(|item| ResultRowPayload::from(item, false, None, false, false))
            .collect();
        let mut filter = ResultFilterState::default();
        filter.replace(1, Arc::clone(&items), rows, Some(PresentationMode::Engine));
        for item in items.iter() {
            assert!(!item.is_session_entry());
            assert_eq!(item.effective_category(), item.category);
        }
        assert_eq!(
            filter.categories(),
            [("website", 1), ("bookmark", 1), ("window", 1)]
        );
        let (shown, rows) = filter.visible();
        assert_eq!(shown.len(), 3);
        assert!(rows.iter().all(|row| row.section_kind.is_empty()));
    }

    #[test]
    fn command_more_filter_expands_all_entries_without_best_match() {
        let items = (0..5)
            .map(|index| {
                let mut entry = item(&format!("entry-{index}"), ResultCategory::Other);
                entry
                    .actions
                    .push(crate::result::Action::EnterSearchEngine {
                        prefix: format!("{index} "),
                    });
                entry
            })
            .collect();
        let mut filter = root_filter(items, PresentationMode::RootSearch, &[]);
        assert_eq!(
            filter
                .filters()
                .iter()
                .map(|f| f.key.as_str())
                .collect::<Vec<_>>(),
            ["all", "command"]
        );
        let (_, rows) = filter.visible();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].section_kind, "command");
        assert_eq!(rows[0].section_count, 5);
        assert_eq!(rows[0].section_more_key, "command");
        assert!(rows.iter().all(|row| !row.is_best_match));
        filter.selected = "command".into();
        let (_, rows) = filter.visible();
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[0].section_kind, "command");
        assert!(rows.iter().all(|row| row.section_more_key.is_empty()));
    }

    #[test]
    fn bookmarks_and_windows_have_distinct_filters_and_row_badges() {
        let items = Arc::new(vec![
            item("bookmark", ResultCategory::Bookmark),
            item("window", ResultCategory::Window),
        ]);
        let rows = items
            .iter()
            .map(|item| ResultRowPayload::from(item, false, None, false, false))
            .collect();
        let mut filter = ResultFilterState::default();
        filter.replace(1, Arc::clone(&items), rows, None);
        assert_eq!(filter.categories(), [("bookmark", 1), ("window", 1)]);
        assert_eq!(filter.all_rows[0].category_kind, "bookmark");
        assert_eq!(filter.all_rows[1].category_kind, "window");
        assert!(filter.all_rows[0].shortcut_prefix.is_empty());
        assert!(filter.all_rows[1].shortcut_prefix.is_empty());
        filter.selected = "window".into();
        assert_eq!(filter.visible().0[0].category, ResultCategory::Window);
    }

    #[test]
    fn window_session_entry_uses_shortcut_hint_without_window_type_label() {
        let entry = item("windows:entry", ResultCategory::Other);
        let row = ResultRowPayload::from(&entry, false, Some("< "), false, false);
        assert!(row.category_kind.is_empty());
        assert_eq!(row.shortcut_prefix, "<");
        let actual = ResultRowPayload::from(
            &item("window:1:2", ResultCategory::Window),
            false,
            None,
            false,
            false,
        );
        assert_eq!(actual.category_kind, "window");
        assert!(actual.shortcut_prefix.is_empty());
    }

    #[test]
    fn bookmark_subtitle_keeps_source_and_domain_until_advanced_details() {
        let mut bookmark = item("bookmark", ResultCategory::Bookmark);
        bookmark.location = "Firefox · docs.example.com".into();
        bookmark.launch_target = PathBuf::from("https://docs.example.com/guide");
        bookmark.launch_entries.push(crate::result::LaunchEntry {
            source: SourceId::Shortcut,
            path: PathBuf::new(),
            target: bookmark.launch_target.clone(),
            arguments: String::new(),
            working_directory: None,
            icon: None,
        });
        assert_eq!(
            display_location(&bookmark, false),
            "Firefox · docs.example.com"
        );
        assert_eq!(
            display_location(&bookmark, true),
            "https://docs.example.com/guide"
        );
    }

    #[test]
    fn unclassified_results_share_the_other_filter() {
        let items = Arc::new(vec![
            item("unknown", ResultCategory::Unclassified),
            item("other", ResultCategory::Other),
            item("app", ResultCategory::Application),
        ]);
        let rows = items
            .iter()
            .map(|item| ResultRowPayload::from(item, false, None, false, false))
            .collect();
        let mut filter = ResultFilterState::default();
        filter.replace(1, items, rows, None);
        assert_eq!(filter.categories(), [("application", 1), ("other", 2)]);
        filter.selected = "other".into();
        assert_eq!(filter.visible().0.len(), 2);
    }

    fn root_filter(
        items: Vec<SearchResult>,
        mode: PresentationMode,
        fallback_ids: &[&str],
    ) -> ResultFilterState {
        let items = Arc::new(items);
        let rows = items
            .iter()
            .map(|item| {
                ResultRowPayload::from(
                    item,
                    false,
                    None,
                    item.id.starts_with("fav"),
                    fallback_ids.contains(&item.id.as_str()),
                )
            })
            .collect();
        let mut filter = ResultFilterState::default();
        filter.replace(1, items, rows, Some(mode));
        filter
    }

    #[test]
    fn root_search_groups_by_first_ranked_category_and_keeps_best_once() {
        let items = vec![
            item("best-web", ResultCategory::Website),
            item("app-1", ResultCategory::Application),
            item("app-2", ResultCategory::Application),
            item("app-3", ResultCategory::Application),
            item("app-4", ResultCategory::Application),
            item("book-1", ResultCategory::Bookmark),
            item("book-2", ResultCategory::Bookmark),
            item("fallback", ResultCategory::Other),
        ];
        let filter = root_filter(items, PresentationMode::RootSearch, &["fallback"]);
        let (shown, rows) = filter.visible();
        assert_eq!(
            shown.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(),
            ["best-web", "app-1", "app-2", "app-3", "book-1", "book-2", "fallback"]
        );
        assert_eq!(
            shown.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(),
            rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>()
        );
        assert_eq!(filter.first_visible_item().unwrap().id, shown[0].id);
        assert_eq!(shown.iter().filter(|i| i.id == "best-web").count(), 1);
        assert_eq!(
            shown
                .iter()
                .zip(&rows)
                .filter(|(_, r)| r.is_best_match)
                .count(),
            1
        );
        assert_eq!(
            rows.iter()
                .map(|r| r.section_kind.as_str())
                .collect::<Vec<_>>(),
            [
                "best_match",
                "application",
                "",
                "",
                "bookmark",
                "",
                "fallbacks"
            ]
        );
        assert_eq!(rows[1].section_count, 4);
        assert_eq!(rows[1].section_more_key, "application");
        assert_eq!(rows[2].section_more_key, "");
        assert_eq!(rows[1].headers_before, 1);
        assert_eq!(rows[0].headers_before, 0);
        assert_eq!(rows[2].headers_before, 2);
        assert_eq!(rows[3].headers_before, 2);
        assert_eq!(rows[4].headers_before, 2);
        assert_eq!(rows[5].headers_before, 3);
        assert_eq!(rows[6].headers_before, 3);
    }

    #[test]
    fn root_search_best_match_skips_fallbacks_and_engine_entries() {
        let mut entry = item("engine-entry", ResultCategory::Other);
        entry
            .actions
            .push(crate::result::Action::EnterSearchEngine {
                prefix: "bm ".into(),
            });
        let filter = root_filter(
            vec![
                item("fallback-top", ResultCategory::Application),
                entry,
                item("real-bookmark", ResultCategory::Bookmark),
                item("fallback-bottom", ResultCategory::Website),
            ],
            PresentationMode::RootSearch,
            &["fallback-top", "fallback-bottom"],
        );
        let (shown, rows) = filter.visible();
        assert_eq!(
            shown.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(),
            [
                "real-bookmark",
                "engine-entry",
                "fallback-top",
                "fallback-bottom"
            ]
        );
        assert_eq!(
            rows.iter()
                .filter(|row| row.is_best_match)
                .map(|row| row.id.as_str())
                .collect::<Vec<_>>(),
            ["real-bookmark"]
        );
        assert_eq!(
            rows.iter()
                .map(|row| row.section_kind.as_str())
                .collect::<Vec<_>>(),
            ["best_match", "command", "fallbacks", ""]
        );
    }

    #[test]
    fn weak_match_rows_do_not_take_the_best_match_slot() {
        let items = Arc::new(vec![
            item("weak", ResultCategory::Application),
            item("strong", ResultCategory::Website),
        ]);
        let mut rows = items
            .iter()
            .map(|item| ResultRowPayload::from(item, false, None, false, false))
            .collect::<Vec<_>>();
        // SearchCore marks the leading weak match as ineligible while the
        // later strong match remains eligible.
        rows[0].best_match_eligible = false;
        let mut filter = ResultFilterState::default();
        filter.replace(1, items, rows, Some(PresentationMode::RootSearch));
        let (shown, rows) = filter.visible();
        assert_eq!(shown[0].id, "strong");
        assert_eq!(rows[0].section_kind, "best_match");
        assert_eq!(rows.iter().filter(|row| row.is_best_match).count(), 1);
    }

    #[test]
    fn only_weak_matches_leave_the_best_match_slot_empty() {
        let items = Arc::new(vec![
            item("weak-a", ResultCategory::Application),
            item("weak-b", ResultCategory::Website),
        ]);
        let mut rows = items
            .iter()
            .map(|item| ResultRowPayload::from(item, false, None, false, false))
            .collect::<Vec<_>>();
        for row in &mut rows {
            row.best_match_eligible = false;
        }
        let mut filter = ResultFilterState::default();
        filter.replace(1, items, rows, Some(PresentationMode::RootSearch));
        let (shown, rows) = filter.visible();
        assert_eq!(shown.len(), 2);
        assert!(rows.iter().all(|row| !row.is_best_match));
        assert_eq!(rows[0].section_kind, "application");
    }

    #[test]
    fn all_fallback_results_have_no_best_match() {
        let filter = root_filter(
            vec![
                item("fallback-1", ResultCategory::Website),
                item("fallback-2", ResultCategory::Other),
            ],
            PresentationMode::RootSearch,
            &["fallback-1", "fallback-2"],
        );
        let (shown, rows) = filter.visible();
        assert!(rows.iter().all(|row| !row.is_best_match));
        assert_eq!(
            shown
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["fallback-1", "fallback-2"]
        );
        assert_eq!(
            rows.iter()
                .map(|row| row.section_kind.as_str())
                .collect::<Vec<_>>(),
            ["fallbacks", ""]
        );
    }

    #[test]
    fn capped_single_category_exposes_more_filter_and_filtered_view_is_uncapped() {
        let items = (0..5)
            .map(|n| item(&format!("app-{n}"), ResultCategory::Application))
            .collect();
        let mut filter = root_filter(items, PresentationMode::RootSearch, &[]);
        assert_eq!(
            filter
                .filters()
                .iter()
                .map(|f| f.key.as_str())
                .collect::<Vec<_>>(),
            ["all", "application"]
        );
        assert_eq!(filter.visible().0.len(), 4);
        assert_eq!(filter.visible().1[1].section_more_key, "application");
        filter.selected = "application".into();
        let (shown, rows) = filter.visible();
        assert_eq!(shown.len(), 5);
        assert!(!rows[0].is_best_match);
        assert_eq!(rows[0].section_kind, "application");
        assert!(rows[1..]
            .iter()
            .all(|r| r.section_kind.is_empty() && r.section_more_key.is_empty()));
        assert_eq!(
            filter
                .filters()
                .iter()
                .map(|f| f.key.as_str())
                .collect::<Vec<_>>(),
            ["all", "application"]
        );
        filter.selected = "all".into();
        assert_eq!(filter.visible().0.len(), 4);
        assert_eq!(filter.first_visible_item().unwrap().id, "app-0");
        filter.selected = "application".into();
        let fewer = Arc::new(vec![
            item("app-a", ResultCategory::Application),
            item("app-b", ResultCategory::Application),
        ]);
        filter.replace(
            2,
            Arc::clone(&fewer),
            fewer
                .iter()
                .map(|item| ResultRowPayload::from(item, false, None, false, false))
                .collect(),
            Some(PresentationMode::RootSearch),
        );
        assert_eq!(filter.selected, "application");
        assert_eq!(
            filter
                .filters()
                .iter()
                .map(|f| f.key.as_str())
                .collect::<Vec<_>>(),
            ["all", "application"]
        );
    }

    #[test]
    fn empty_root_orders_favorites_tools_then_suggestions_without_cap() {
        let suggestion = item("suggestion", ResultCategory::Application);
        let mut tool = item("tool", ResultCategory::Other);
        tool.actions.push(crate::result::Action::EnterSearchEngine {
            prefix: "x ".into(),
        });
        let mut items = vec![tool, suggestion];
        for n in 0..5 {
            items.push(item(&format!("fav-{n}"), ResultCategory::Application));
        }
        let filter = root_filter(items, PresentationMode::RootEmpty, &[]);
        let (shown, rows) = filter.visible();
        assert_eq!(shown.len(), 7);
        assert!(shown[0].id.starts_with("fav"));
        assert_eq!(rows[0].section_kind, "favorites");
        assert_eq!(rows[5].section_kind, "command");
        assert_eq!(rows[6].section_kind, "suggestions");
        assert!(rows
            .iter()
            .all(|r| !r.is_best_match && r.section_more_key.is_empty()));
    }

    #[test]
    fn engine_mode_preserves_page_order_without_sections_or_cap() {
        let items = (0..6)
            .map(|n| item(&format!("engine-{n}"), ResultCategory::Website))
            .collect();
        let filter = root_filter(items, PresentationMode::Engine, &[]);
        let (shown, rows) = filter.visible();
        assert_eq!(shown.len(), 6);
        assert_eq!(shown[0].id, "engine-0");
        assert!(rows
            .iter()
            .all(|r| r.section_kind.is_empty() && r.headers_before == 0 && !r.is_best_match));
    }
}
