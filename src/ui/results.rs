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
}

#[derive(Default)]
pub(super) struct ResultFilterState {
    pub(super) query_id: u64,
    pub(super) all_items: Arc<Vec<SearchResult>>,
    pub(super) all_rows: Vec<ResultRowPayload>,
    pub(super) selected: String,
}

impl ResultFilterState {
    pub(super) fn replace(
        &mut self,
        query_id: u64,
        items: Arc<Vec<SearchResult>>,
        rows: Vec<ResultRowPayload>,
    ) {
        self.query_id = query_id;
        self.all_items = items;
        self.all_rows = rows;
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
        const ORDER: [&str; 7] = [
            "application",
            "game",
            "website",
            "calculator",
            "control_panel",
            "document",
            "other",
        ];
        let mut counts = [0_usize; 7];
        for item in self.all_items.iter() {
            let key = category_key(item.category);
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
        if categories.len() < 2 {
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
        if self.selected == "all" || self.selected.is_empty() {
            return (Arc::clone(&self.all_items), self.all_rows.clone());
        }
        let mut items = Vec::new();
        let mut rows = Vec::new();
        for (item, row) in self.all_items.iter().zip(&self.all_rows) {
            if category_key(item.category) == self.selected {
                items.push(item.clone());
                rows.push(row.clone());
            }
        }
        (Arc::new(items), rows)
    }

    pub(super) fn first_visible_item(&self) -> Option<&SearchResult> {
        self.all_items.iter().find(|item| {
            self.selected == "all"
                || self.selected.is_empty()
                || category_key(item.category) == self.selected
        })
    }
}

fn category_key(category: ResultCategory) -> &'static str {
    match category {
        ResultCategory::Application => "application",
        ResultCategory::Game => "game",
        ResultCategory::Website => "website",
        ResultCategory::Calculator => "calculator",
        ResultCategory::ControlPanel => "control_panel",
        ResultCategory::Document => "document",
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{platform::SourceId, result::ResultVisibility};

    fn item(id: &str, category: ResultCategory) -> SearchResult {
        SearchResult {
            id: id.into(),
            identity: id.into(),
            title: id.into(),
            category,
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
            .map(|item| ResultRowPayload::from(item, false, None, false))
            .collect();
        let mut filter = ResultFilterState::default();
        filter.replace(1, items, rows);
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
                .map(|item| ResultRowPayload::from(item, false, None, false))
                .collect(),
        );
        assert!(filter.filters().is_empty());
        assert_eq!(filter.selected, "all");
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
            .map(|item| ResultRowPayload::from(item, false, None, false))
            .collect();
        let mut filter = ResultFilterState::default();
        filter.replace(1, items, rows);
        assert_eq!(filter.categories(), [("application", 1), ("other", 2)]);
        filter.selected = "other".into();
        assert_eq!(filter.visible().0.len(), 2);
    }
}
