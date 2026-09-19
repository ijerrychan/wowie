use super::{Launcher, ResultRow};
use crate::{
    icon_service::IconService,
    result::{ResultCategory, SearchResult},
};
use slint::Model;
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
    category: String,
}

impl ResultRowPayload {
    pub(super) fn from(item: &SearchResult, show_advanced_details: bool) -> Self {
        let location = display_location(item, show_advanced_details);
        Self {
            id: item.id.clone(),
            title: item.title.clone(),
            location,
            location_kind: if !show_advanced_details && is_store_application_shell_target(item) {
                "apps-folder-entry"
            } else {
                ""
            }
            .into(),
            icon: None,
            category: match item.category {
                ResultCategory::Game => "game",
                ResultCategory::Application => "application",
                ResultCategory::Website => "website",
                ResultCategory::Shortcut => "shortcut",
                ResultCategory::Calculator => "calculator",
                ResultCategory::ControlPanel => "control-panel",
                ResultCategory::Document => "document",
                ResultCategory::Other => "other",
                ResultCategory::Unclassified => "application",
            }
            .into(),
        }
    }
}

fn display_location(item: &SearchResult, show_advanced_details: bool) -> String {
    if !show_advanced_details && item.category == ResultCategory::ControlPanel {
        return String::new();
    }
    let Some(entry) = crate::providers::app::preferred_launch_entry(item) else {
        return item.location.clone();
    };
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

fn is_store_application_shell_target(item: &SearchResult) -> bool {
    item.location
        .trim_start()
        .get(.."shell:AppsFolder\\".len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("shell:AppsFolder\\"))
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
            if state.1.insert(index) {
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
            let Some(icon) = icon_service.resolve(&item) else {
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
            category: payload.category.into(),
        }
    }
}
