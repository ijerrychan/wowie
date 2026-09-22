use super::{Launcher, ResultRow};
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
            crate::platform::application_description(&entry.target).unwrap_or_default()
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
    std::thread::spawn(move || {
        let mut resolved = Vec::with_capacity(pending.len());
        for (index, item) in pending {
            if query_generation.load(Ordering::Acquire) != query_id {
                return;
            }
            let Some(icon) = icon_service.resolve_at(&item, DISPLAY_ICON_SIZE) else {
                continue;
            };
            resolved.push((index, icon));
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
            for (index, icon) in resolved {
                let Some(mut row) = model.row_data(index) else {
                    continue;
                };
                row.icon = icon_to_image(icon.as_ref());
                model.set_row_data(index, row);
            }
        });
    });
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
