use super::*;

pub(super) fn apply_result_filter(
    window: &Launcher,
    engine: &search::SearchCore,
    filter: &results::ResultFilterState,
    icon_items: &Mutex<(u64, Arc<Vec<SearchResult>>)>,
    icon_requests: &Mutex<(u64, HashSet<usize>)>,
) -> (bool, usize) {
    let category_filters = filter.filters();
    let selected_category_index = category_filters
        .iter()
        .position(|option| option.key == filter.selected.as_str())
        .unwrap_or(0) as i32;
    window.set_category_filters(ModelRc::from(Rc::new(VecModel::from(category_filters))));
    window.set_selected_category_filter(filter.selected.clone().into());
    window.set_selected_category_index(selected_category_index);
    let (visible_items, rows) = filter.visible();
    let changed = !matches_model(&window.get_results(), &rows);
    let count = rows.len();
    engine.publish_results(Arc::clone(&visible_items));
    if let Ok(mut state) = icon_items.lock() {
        *state = (filter.query_id, visible_items);
    }
    if let Ok(mut state) = icon_requests.lock() {
        *state = (filter.query_id, HashSet::new());
    }
    if changed {
        window.set_results(ModelRc::from(Rc::new(VecModel::from(
            rows.into_iter().map(ResultRow::from).collect::<Vec<_>>(),
        ))));
    }
    (changed, count)
}

pub(super) fn execute_context_plugin_action(
    engine: Arc<search::SearchCore>,
    item: SearchResult,
    action_id: String,
    weak: slint::Weak<Launcher>,
    generation: Arc<AtomicU64>,
) {
    let request_generation = generation.load(Ordering::Acquire);
    crate::utils::background::spawn(move || {
        let result = engine.execute_transient_plugin_action(&item, &action_id);
        let _ = slint::invoke_from_event_loop(move || {
            if generation.load(Ordering::Acquire) != request_generation {
                return;
            }
            let Some(window) = weak.upgrade() else {
                return;
            };
            match result {
                Ok(output) => {
                    window.set_last_action_id(item.id.into());
                    window.set_last_action_result(output.title.into());
                    window.set_last_action_subtitle(output.subtitle.into());
                    window.set_launch_error(String::new().into());
                }
                Err(error) => window.set_launch_error(error.to_string().into()),
            }
        });
    });
}

/// Preserve page order while excluding IDs already shown or repeated in the
/// page. The same surface limit applies to both initial and paginated results.
pub(super) fn accepted_page_items(
    existing: &[SearchResult],
    more: Vec<SearchResult>,
) -> Vec<SearchResult> {
    let mut seen = existing
        .iter()
        .map(|item| item.id.clone())
        .collect::<HashSet<_>>();
    more.into_iter()
        .filter(|item| seen.insert(item.id.clone()))
        .take(MAX_RENDERED_RESULTS.saturating_sub(existing.len()))
        .collect()
}

/// Clear the shared query and leave any active search-engine session.
///
/// Successful terminal actions use this single path so the next launcher
/// invocation always starts from the default engine with an empty input.
pub(super) fn clear_query(
    window: &Launcher,
    active_session: &Mutex<Option<crate::page::SearchEngineSession>>,
) {
    window.set_shortcut_session_active(false);
    window.set_shortcut_session_execute_mode(false);
    window.set_shortcut_session_prefix(slint::SharedString::default());
    window.set_shortcut_session_placeholder(slint::SharedString::default());
    if let Ok(mut session) = active_session.lock() {
        *session = None;
    }
    let weak = window.as_weak();
    Timer::single_shot(Duration::ZERO, move || {
        if let Some(window) = weak.upgrade() {
            let query = slint::SharedString::default();
            window.set_query_text(query.clone());
            window.invoke_query_changed(query);
        }
    });
}

pub(super) fn allow_favorite_in_context(
    session: Option<&crate::page::SearchEngineSession>,
    session_active: bool,
    query: &str,
) -> bool {
    if !session_active || query.trim().is_empty() {
        return true;
    }
    session.is_some_and(|session| session.mode != crate::page::SearchEngineMode::Execute)
}

pub(super) fn can_favorite_item(
    item: &SearchResult,
    session: Option<&crate::page::SearchEngineSession>,
    session_active: bool,
    query: &str,
) -> bool {
    item.allow_favorite && allow_favorite_in_context(session, session_active, query)
}

pub(super) fn explicit_detail_matches(
    open_item_id: Option<&str>,
    selected_item_id: Option<&str>,
) -> bool {
    open_item_id.is_some_and(|open_item_id| Some(open_item_id) == selected_item_id)
}

/// Apply the effective result-surface height, honoring compact empty search.
/// Empty-query entries arrive asynchronously, so relying solely on the input
/// callback can leave the native window at the wrong height.
pub(super) fn restore_result_window_height(window: &Launcher) -> bool {
    let target_height = window.get_effective_window_height();
    let native_window = window.window();
    let logical_size = native_window
        .size()
        .to_logical(native_window.scale_factor());
    let resized = (logical_size.height - target_height).abs() > 0.5;
    if resized {
        platform::set_window_background(native_window, window.get_native_background_color());
        native_window.set_size(slint::LogicalSize::new(logical_size.width, target_height));
        native_window.request_redraw();
    }
    resized
}

pub(super) fn schedule_visible_range_refresh(window: &Launcher, end: i32) {
    let weak = window.as_weak();
    Timer::single_shot(Duration::ZERO, move || {
        if let Some(window) = weak.upgrade() {
            window.invoke_visible_range_changed(0, end.max(0));
        }
    });
}

/// Per-query continuation state for paged search engines.
#[derive(Default)]
pub(super) struct PageLoadState {
    pub(super) query_id: u64,
    pub(super) loading: bool,
    pub(super) exhausted: bool,
}

#[derive(Clone)]
pub(super) struct QueryHistoryEntry {
    pub(super) query: String,
    pub(super) session: Option<crate::page::SearchEngineSession>,
}

pub(super) fn record_query_history(
    history: &Mutex<(Vec<QueryHistoryEntry>, usize)>,
    query: &str,
    session: Option<crate::page::SearchEngineSession>,
) {
    if query.trim().is_empty() {
        return;
    }
    if let Ok(mut history) = history.lock() {
        if history
            .0
            .last()
            .is_none_or(|entry| entry.query != query || entry.session != session)
        {
            history.0.push(QueryHistoryEntry {
                query: query.to_owned(),
                session,
            });
        }
        history.1 = history.0.len();
    }
}

pub(super) fn update_search_icon(
    window: &Launcher,
    icon: Option<&crate::platform::IconData>,
    kind: String,
    is_shortcut: bool,
    animate: bool,
    transition_generation: Arc<AtomicU64>,
) {
    let transition_id = transition_generation.fetch_add(1, Ordering::AcqRel) + 1;
    let was_shortcut = window.get_search_icon_is_shortcut();
    let should_transition = animate && (was_shortcut || is_shortcut);
    let previous_icon = window.get_search_icon();
    let previous_kind = window.get_search_icon_kind();
    if should_transition {
        window.set_search_icon_animation_enabled(false);
        window.set_search_icon_transition_progress(0.0);
        window.set_search_icon_previous(previous_icon);
        window.set_search_icon_previous_kind(previous_kind);
    }
    window.set_search_icon(icon.map(results::icon_to_image).unwrap_or_default());
    window.set_search_icon_kind(kind.into());
    window.set_search_icon_is_shortcut(is_shortcut);

    if should_transition {
        // Let the reset state render for one frame. Without this separation,
        // Slint can coalesce reset and activation into one frame.
        let weak = window.as_weak();
        Timer::single_shot(SEARCH_ICON_FRAME_DELAY, move || {
            if let Some(window) = weak.upgrade() {
                if transition_generation.load(Ordering::Acquire) != transition_id {
                    return;
                }
                window.set_search_icon_animation_enabled(true);
                window.set_search_icon_transition_progress(1.0);
            }
        });
    } else {
        window.set_search_icon_animation_enabled(false);
        window.set_search_icon_transition_progress(1.0);
        window.set_search_icon_animation_enabled(true);
    }
}

pub(super) struct SearchPresentation {
    pub(super) found: Arc<Vec<SearchResult>>,
    pub(super) rows: Vec<ResultRowPayload>,
    pub(super) shortcut_icon: Option<Arc<crate::platform::IconData>>,
    pub(super) shortcut_icon_kind: String,
    pub(super) is_search_engine: bool,
    pub(super) search_engine: Option<crate::page::SearchEngineSession>,
    pub(super) empty_state: Option<SearchResult>,
}

#[derive(Clone, Copy)]
pub(super) enum SelectionPolicy {
    First,
    Preserve,
    KeepIndex,
}

pub(super) struct ResultSnapshot {
    pub(super) query_id: u64,
    pub(super) items: Arc<Vec<SearchResult>>,
    pub(super) rows: Vec<ResultRowPayload>,
    pub(super) expected_previous_len: Option<usize>,
    pub(super) selection: SelectionPolicy,
    pub(super) show_results: bool,
}

pub(super) struct SnapshotTargets<'a> {
    pub(super) engine: &'a search::SearchCore,
    pub(super) filter: &'a Mutex<results::ResultFilterState>,
    pub(super) icon_items: &'a results::IconItemState,
    pub(super) icon_requests: &'a Arc<Mutex<(u64, HashSet<usize>)>>,
    pub(super) query_generation: &'a Arc<AtomicU64>,
}

pub(super) fn apply_result_snapshot(
    window: &Launcher,
    snapshot: ResultSnapshot,
    targets: SnapshotTargets<'_>,
) -> Option<(bool, usize)> {
    if targets.query_generation.load(Ordering::Acquire) != snapshot.query_id {
        return None;
    }
    let current_index = window.get_selected_index();
    let selected_id = matches!(snapshot.selection, SelectionPolicy::Preserve)
        .then(|| window.get_results().row_data(current_index.max(0) as usize))
        .flatten()
        .map(|row| row.id.to_string());
    let (changed, count) = {
        let Ok(mut filter) = targets.filter.lock() else {
            return None;
        };
        if snapshot.expected_previous_len.is_some_and(|len| {
            filter.query_id != snapshot.query_id || filter.all_items.len() != len
        }) {
            return None;
        }
        filter.replace(snapshot.query_id, snapshot.items, snapshot.rows);
        apply_result_filter(
            window,
            targets.engine,
            &filter,
            targets.icon_items,
            targets.icon_requests,
        )
    };
    if changed {
        let selected_index = if matches!(snapshot.selection, SelectionPolicy::KeepIndex) {
            current_index
        } else if snapshot.show_results && count > 0 {
            selected_id
                .and_then(|id| {
                    (0..count).find(|index| {
                        window
                            .get_results()
                            .row_data(*index)
                            .is_some_and(|row| row.id == id)
                    })
                })
                .map(|index| index as i32)
                .unwrap_or(0)
        } else {
            -1
        };
        window.set_selected_index(selected_index);
        window.invoke_selection_changed(selected_index);
    }
    Some((changed, count))
}

pub(super) struct PresentationTargets<'a> {
    pub(super) engine: &'a search::SearchCore,
    pub(super) icon_service: &'a IconService,
    pub(super) filter: &'a Mutex<results::ResultFilterState>,
    pub(super) icon_items: &'a results::IconItemState,
    pub(super) icon_requests: &'a Arc<Mutex<(u64, HashSet<usize>)>>,
    pub(super) empty_state_item: &'a Mutex<Option<SearchResult>>,
    pub(super) detail_registry: &'a crate::details::DetailRegistry,
    pub(super) query_generation: &'a Arc<AtomicU64>,
    pub(super) search_icon_generation: &'a Arc<AtomicU64>,
}

pub(super) fn apply_search_presentation(
    window: &Launcher,
    query_id: u64,
    query: &str,
    presentation: SearchPresentation,
    selection: SelectionPolicy,
    targets: PresentationTargets<'_>,
) {
    if targets.query_generation.load(Ordering::Acquire) != query_id {
        return;
    }
    let SearchPresentation {
        found,
        rows,
        shortcut_icon,
        shortcut_icon_kind,
        is_search_engine,
        search_engine,
        empty_state,
    } = presentation;
    apply_search_engine(window, search_engine.as_ref());
    apply_empty_state(window, empty_state.as_ref(), targets.icon_service);
    if let Ok(mut state) = targets.empty_state_item.lock() {
        *state = empty_state;
    }
    let show_results =
        !query.trim().is_empty() || is_search_engine || window.get_empty_query_results_visible();
    let Some((results_changed, count)) = apply_result_snapshot(
        window,
        ResultSnapshot {
            query_id,
            items: found,
            rows,
            expected_previous_len: None,
            selection,
            show_results,
        },
        SnapshotTargets {
            engine: targets.engine,
            filter: targets.filter,
            icon_items: targets.icon_items,
            icon_requests: targets.icon_requests,
            query_generation: targets.query_generation,
        },
    ) else {
        return;
    };
    let (detail, detail_panel) = targets
        .filter
        .lock()
        .map(|filter| first_result_detail(&filter, targets.detail_registry))
        .unwrap_or((None, false));
    let icon_mode_changed = window.get_search_icon_is_shortcut() != is_search_engine;
    let search_icon_changed = icon_mode_changed || (results_changed && is_search_engine);
    if results_changed && is_search_engine {
        schedule_visible_range_refresh(window, count.min(8) as i32);
    }
    if search_icon_changed {
        update_search_icon(
            window,
            shortcut_icon.as_deref(),
            shortcut_icon_kind,
            is_search_engine,
            icon_mode_changed,
            Arc::clone(targets.search_icon_generation),
        );
    }
    if results_changed || detail.is_some() || detail_panel {
        apply_search_detail_state(window, detail.as_ref(), detail_panel, targets.icon_service);
    }
    if !query.trim().is_empty() || is_search_engine {
        restore_result_window_height(window);
    }
    window.set_query_results_ready(true);
    request_icons(
        0,
        count.min(8) as i32,
        targets.icon_items,
        targets.icon_requests,
        targets.icon_service,
        &window.as_weak(),
        targets.query_generation,
    );
}

pub(super) fn first_result_detail(
    filter: &results::ResultFilterState,
    registry: &crate::details::DetailRegistry,
) -> (Option<result::DetailView>, bool) {
    let Some(item) = filter.first_visible_item() else {
        return (None, false);
    };
    if registry.provider_for(item).is_some() {
        return (None, false);
    }
    let show_panel =
        item.detail.is_some() && item.visibility != result::ResultVisibility::EmptyQueryOnly;
    (item.detail.clone(), show_panel)
}

pub(super) fn build_search_presentation(
    engine: &search::SearchCore,
    query: &str,
    active_session: Option<&crate::page::SearchEngineSession>,
    show_advanced_details: bool,
    icon_service: &IconService,
) -> SearchPresentation {
    let search_context = active_session
        .cloned()
        .map(crate::page::SearchContext::Engine)
        .unwrap_or_else(|| engine.search_context(query));
    let found = engine.search_in_context_limited(&search_context, query, MAX_RENDERED_RESULTS);
    let empty_state = found
        .is_empty()
        .then(|| engine.empty_state(&search_context, query))
        .flatten();
    let search_engine = search_context.session().cloned();
    // SearchCore gives an entered engine exclusive ownership of the result
    // surface. The UI never identifies engine rows by their result category.
    let search_engine_entry = search_engine
        .as_ref()
        .and_then(|session| engine.search_engine_result(session));
    let shortcut_icon = search_engine_entry
        .as_ref()
        .filter(|item| static_icon_kind(item.icon.as_ref()).is_empty())
        .and_then(|item| icon_service.resolve_at(item, DISPLAY_ICON_SIZE));
    let shortcut_icon_kind = search_engine_entry
        .as_ref()
        .map(|item| static_icon_kind(item.icon.as_ref()).to_owned())
        .unwrap_or_default();
    let is_search_engine = search_engine.is_some();
    let rows = found
        .iter()
        .map(|item| {
            // A session prefix selects the engine, not every result the
            // engine returns. Showing it on menu rows is both misleading and
            // consumes their trailing-row space.
            let shortcut_hint = (!is_search_engine && active_session.is_none())
                .then(|| engine.shortcut_hint(&item.id))
                .flatten();
            ResultRowPayload::from(
                item,
                show_advanced_details,
                shortcut_hint.as_deref(),
                engine.is_favorite(&item.id),
            )
        })
        .collect();
    SearchPresentation {
        found: Arc::new(found),
        rows,
        shortcut_icon,
        shortcut_icon_kind,
        is_search_engine,
        search_engine,
        empty_state,
    }
}

pub(super) fn apply_empty_state(
    window: &Launcher,
    state: Option<&SearchResult>,
    icon_service: &IconService,
) {
    let generation = window.get_empty_state_icon_generation().wrapping_add(1);
    window.set_empty_state_icon_generation(generation);
    window.set_empty_state_detail_active(false);
    if let Some(state) = state {
        window.set_empty_state_title(state.title.clone().into());
        window.set_empty_state_description(
            state
                .description
                .as_deref()
                .unwrap_or(&state.location)
                .into(),
        );
        let icon_kind = static_icon_kind(state.icon.as_ref());
        window.set_empty_state_icon_kind(icon_kind.into());
        window.set_empty_state_icon(slint::Image::default());
        if icon_kind.is_empty() {
            let item = state.clone();
            let service = icon_service.clone();
            let weak = window.as_weak();
            crate::utils::background::spawn(move || {
                let Some(icon) = service.resolve_at(&item, DISPLAY_ICON_SIZE) else {
                    return;
                };
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(window) = weak.upgrade() else {
                        return;
                    };
                    if window.get_empty_state_icon_generation() == generation {
                        window.set_empty_state_icon(results::icon_to_image(&icon));
                    }
                });
            });
        }
    } else {
        window.set_empty_state_title(String::new().into());
        window.set_empty_state_description(String::new().into());
        window.set_empty_state_icon_kind(String::new().into());
        window.set_empty_state_icon(slint::Image::default());
    }
}

pub(super) fn apply_search_detail_state(
    window: &Launcher,
    detail: Option<&result::DetailView>,
    show_panel: bool,
    icon_service: &IconService,
) {
    set_detail_view(window, detail, icon_service);
    // The detail surface is opened only by an explicit launch request, not as
    // a side effect of moving selection.
    window.set_detail_panel_visible(show_panel);
}

pub(super) fn apply_search_engine(
    window: &Launcher,
    session: Option<&crate::page::SearchEngineSession>,
) {
    window.set_shortcut_session_active(session.is_some());
    window.set_shortcut_session_execute_mode(
        session.is_some_and(|session| session.mode == crate::page::SearchEngineMode::Execute),
    );
    window.set_shortcut_session_prefix(
        session
            .map(|session| session.prefix.clone().into())
            .unwrap_or_default(),
    );
    window.set_shortcut_session_placeholder(
        session
            .map(|session| {
                if session.placeholder.trim().is_empty() {
                    session.title.clone()
                } else {
                    session.placeholder.clone()
                }
            })
            .unwrap_or_default()
            .into(),
    );
}
