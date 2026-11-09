use super::*;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType, Rgb565Pixel};
use slint::platform::{Platform, PlatformError, WindowAdapter};

struct LayoutPlatform {
    windows: Rc<std::cell::RefCell<Vec<Rc<MinimalSoftwareWindow>>>>,
    next_window: Rc<Cell<usize>>,
    diagnostics: Rc<std::cell::RefCell<Vec<String>>>,
}

impl Platform for LayoutPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        let index = self.next_window.get();
        self.next_window.set(index + 1);
        if index >= self.windows.borrow().len() {
            self.windows
                .borrow_mut()
                .push(MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer));
        }
        Ok(self.windows.borrow()[index].clone())
    }

    fn debug_log(&self, arguments: std::fmt::Arguments) {
        self.diagnostics.borrow_mut().push(arguments.to_string());
    }
}

/// Selection positions that exercise row clipping: both ends, the middle and
/// both sides of every section boundary. The exhaustive sweep is reserved for
/// the baseline; fractional-DPI and translated runs use the same boundaries
/// without repeating the full traversal.
fn grouped_selections(
    groups: &[(usize, &str, i32, &str)],
    count: usize,
    exhaustive: bool,
) -> Vec<usize> {
    let mut boundaries = Vec::new();
    for (start, _, _, _) in groups.iter().filter(|(start, _, _, _)| *start < count) {
        if exhaustive {
            for offset in [-1_isize, 0, 1] {
                if let Some(index) = start.checked_add_signed(offset) {
                    if index < count {
                        boundaries.push(index);
                    }
                }
            }
        } else {
            if *start > 0 {
                boundaries.push(*start - 1);
            }
            boundaries.push(*start);
        }
    }
    boundaries.sort_unstable_by(|left, right| right.cmp(left));
    // Start at the top, jump to the bottom, then walk upward through the
    // sampled boundaries. This preserves the scroll-up clipping regression
    // path instead of only scrolling downward.
    let mut selections = vec![0, count - 1, count / 2];
    for index in boundaries {
        if !selections.contains(&index) {
            selections.push(index);
        }
    }
    selections
}

/// Render one selected row and assert that the selection highlight is not
/// clipped and that no callback chain was logged.
fn assert_selected_row_visible(
    window: &Launcher,
    native: &MinimalSoftwareWindow,
    diagnostics: &std::cell::RefCell<Vec<String>>,
    width: u32,
    height: u32,
    scale: f32,
    selected: usize,
    context: &str,
) {
    window.set_selected_index(selected as i32);
    assert_eq!(window.get_selected_index(), selected as i32);
    window.window().request_redraw();
    let mut pixels = vec![slint::Rgb8Pixel::default(); width as usize * height as usize];
    native.draw_if_needed(|renderer| {
        renderer.render(&mut pixels, width as usize);
    });
    let selected_color_rows = pixels
        .chunks(width as usize)
        .filter(|row| {
            row.iter()
                .filter(|pixel| pixel.r == 229 && pixel.g == 0 && pixel.b == 200)
                .count()
                > width as usize / 3
        })
        .count();
    let minimum_selected_height = (window.get_list_item_height() * scale).floor() as usize - 2;
    assert!(
        selected_color_rows >= minimum_selected_height,
        "selected row was not visible: {context}, selected={selected}, \
         colored_rows={selected_color_rows}, expected_at_least={minimum_selected_height}"
    );
    let messages = diagnostics.borrow();
    assert!(
        !messages
            .iter()
            .any(|message| message.contains("long callback/instantiation")),
        "grouped list did not settle: {context}, selected={selected}: {messages:?}"
    );
}

fn save_current_software_preview(
    native: &MinimalSoftwareWindow,
    path: &str,
    width: u32,
    height: u32,
) {
    let mut pixels = vec![slint::Rgb8Pixel::default(); width as usize * height as usize];
    native.draw_if_needed(|renderer| {
        renderer.render(&mut pixels, width as usize);
    });
    let rgba = pixels
        .iter()
        .flat_map(|pixel| [pixel.r, pixel.g, pixel.b, 255])
        .collect::<Vec<_>>();
    let preview = std::path::Path::new("target/previews").join(path);
    std::fs::create_dir_all(preview.parent().unwrap()).unwrap();
    image::RgbaImage::from_raw(width, height, rgba)
        .unwrap()
        .save(preview)
        .unwrap();
}

fn selected_highlight_right_edge(
    native: &MinimalSoftwareWindow,
    width: u32,
    height: u32,
) -> Option<u32> {
    let mut pixels = vec![slint::Rgb8Pixel::default(); width as usize * height as usize];
    native.draw_if_needed(|renderer| {
        renderer.render(&mut pixels, width as usize);
    });
    pixels
        .iter()
        .enumerate()
        .filter(|(_, pixel)| pixel.r == 229 && pixel.g == 0 && pixel.b == 200)
        .map(|(index, _)| (index % width as usize) as u32)
        .max()
}

#[test]
fn launcher_layout_settles_when_reopened() {
    // Use the actual compiled UI and collect runtime diagnostics without
    // opening native windows or depending on a GPU. The zero-height viewport
    // regression reproduces when a scrolled list changes size in compact mode.
    let native = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    let software_windows = Rc::new(std::cell::RefCell::new(vec![native.clone()]));
    let next_window = Rc::new(Cell::new(0));
    let diagnostics = Rc::new(std::cell::RefCell::new(Vec::new()));
    slint::platform::set_platform(Box::new(LayoutPlatform {
        windows: software_windows.clone(),
        next_window,
        diagnostics: diagnostics.clone(),
    }))
    .unwrap();
    let window = Launcher::new().unwrap();
    // Exercise the production delayed-feedback state using an injected clock:
    // fast completion stays quiet, slow completion shows status, and a newer
    // query or an empty root query invalidates the previous timer.
    let feedback = std::sync::Mutex::new(SearchFeedbackState::default());
    let started = std::time::Instant::now();
    feedback.lock().unwrap().begin(9, "fast", false, started);
    complete_launcher_search_feedback(&window, &feedback, 9);
    assert!(!update_launcher_search_feedback(
        &window,
        &feedback,
        9,
        started + SEARCH_FEEDBACK_DELAY + std::time::Duration::from_secs(1),
    ));
    assert!(!window.get_search_feedback_visible());
    {
        let mut state = feedback.lock().unwrap();
        state.begin(10, "weather", false, started);
    }
    window.set_query_results_ready(false);
    window.set_search_feedback_visible(false);
    assert!(!update_launcher_search_feedback(
        &window,
        &feedback,
        10,
        started + SEARCH_FEEDBACK_DELAY - std::time::Duration::from_millis(1),
    ));
    assert!(!window.get_search_feedback_visible());
    window.set_animations_enabled(false);
    assert!(update_launcher_search_feedback(
        &window,
        &feedback,
        10,
        started + SEARCH_FEEDBACK_DELAY + std::time::Duration::from_millis(1),
    ));
    assert!(window.get_search_feedback_visible());
    window.set_query_results_ready(true);
    complete_launcher_search_feedback(&window, &feedback, 10);
    assert!(!window.get_search_feedback_visible());

    let next_started = started + std::time::Duration::from_secs(1);
    feedback
        .lock()
        .unwrap()
        .begin(11, "next", false, next_started);
    assert!(!update_launcher_search_feedback(
        &window,
        &feedback,
        10,
        next_started + SEARCH_FEEDBACK_DELAY + std::time::Duration::from_secs(1),
    ));
    assert!(!window.get_search_feedback_visible());
    feedback.lock().unwrap().begin(12, "", false, next_started);
    assert!(!update_launcher_search_feedback(
        &window,
        &feedback,
        12,
        next_started + SEARCH_FEEDBACK_DELAY + std::time::Duration::from_secs(1),
    ));
    assert!(!window.get_search_feedback_visible());
    feedback
        .lock()
        .unwrap()
        .begin(13, "engine query", true, next_started);
    assert!(update_launcher_search_feedback(
        &window,
        &feedback,
        13,
        next_started + SEARCH_FEEDBACK_DELAY + std::time::Duration::from_millis(1),
    ));
    complete_launcher_search_feedback(&window, &feedback, 12);
    assert!(window.get_search_feedback_visible());
    // Provider refreshes may advance generation without a separate timer.
    complete_launcher_search_feedback(&window, &feedback, 14);
    assert!(!window.get_search_feedback_visible());
    window.set_animations_enabled(true);
    window.set_native_window_frame(true);
    // The native fallback must use the canvas actually rendered by Slint,
    // including light mode's slightly tinted (rather than pure white) color.
    for dark in [true, false] {
        window.set_dark_mode(dark);
        native.set_size(slint::PhysicalSize::new(900, 350));
        window.show().unwrap();
        window.window().request_redraw();
        native.draw_if_needed(|renderer| {
            let mut pixels = vec![slint::Rgb8Pixel::default(); 900 * 350];
            renderer.render(&mut pixels, 900);
            let canvas = window.get_native_background_color();
            // Native DWM owns the complete contour. A Slint stroke here would
            // be clipped at its rounded corners and create a second outline.
            for index in [450, 175 * 900, 175 * 900 + 899, 349 * 900 + 450] {
                assert_eq!(
                    pixels[index],
                    slint::Rgb8Pixel {
                        r: canvas.red(),
                        g: canvas.green(),
                        b: canvas.blue(),
                    },
                    "native frame must not contain a second client-side border"
                );
            }
            assert_eq!(
                pixels[5 * 900 + 5],
                slint::Rgb8Pixel {
                    r: canvas.red(),
                    g: canvas.green(),
                    b: canvas.blue(),
                }
            );
        });
        window.hide().unwrap();
    }
    let model = Rc::new(VecModel::from(
        (0..MAX_RENDERED_RESULTS)
            .map(|index| ResultRow {
                id: index.to_string().into(),
                title: format!("Result {index}").into(),
                icon_kind: "calculator".into(),
                ..Default::default()
            })
            .collect::<Vec<_>>(),
    ));
    window.set_results(ModelRc::from(model.clone()));
    // Cover the empty/single-row/overflow boundaries and fractional DPI with
    // representative cases instead of rendering the full Cartesian product.
    for (scale, row_height, compact, count) in [
        (1.0, 49.0, false, 0),
        (1.0, 49.0, true, 0),
        (1.0, 49.0, true, 1),
        (1.25, 50.0, true, 8),
        (1.5, 35.0, false, 8),
        (1.75, 50.0, true, 100),
        (2.0, 65.0, false, 256),
        (1.0, 49.0, true, 256),
    ] {
        window
            .window()
            .dispatch_event(slint::platform::WindowEvent::ScaleFactorChanged {
                scale_factor: scale,
            });
        window.set_list_item_height(row_height);
        window.set_compact_search(compact);
        window.set_results(ModelRc::from(Rc::new(VecModel::from(
            (0..count)
                .filter_map(|index| model.row_data(index))
                .collect::<Vec<_>>(),
        ))));
        let mut selections = vec![-1, count as i32 - 1];
        selections.dedup();
        for selected in selections {
            window.set_selected_index(selected);
            // Small window, grow, then shrink back after reopening. The
            // shrink step also catches stale layout state from the larger view.
            for height in [56, 600, 350] {
                window.hide().unwrap();
                native.set_size(slint::PhysicalSize::new(900, height));
                window.show().unwrap();
                window.window().request_redraw();
                native.draw_if_needed(|renderer| {
                    let mut pixels = vec![Rgb565Pixel::default(); 900 * height as usize];
                    renderer.render(&mut pixels, 900);
                });
                let messages = diagnostics.borrow();
                let chain_detected = messages
                    .iter()
                    .any(|message| message.contains("long callback/instantiation"));
                assert!(
                    !chain_detected,
                    "layout did not settle: count={count}, scale={scale}, \
                     row_height={row_height}, compact={compact}, \
                     selected={selected}, height={height}: {messages:?}"
                );
            }
        }
    }

    // Headless grouped-list smoke: lots of virtualized rows, multiple section
    // headers, root-only shortcut/category affordances, and a narrow layout.
    window.set_ui_font_size(20.0);
    window.set_list_item_height(52.0);
    window.set_compact_search(false);
    window.set_accent_background(slint::Color::from_rgb_u8(229, 0, 200));
    let groups = [
        (0, "best_match", 1, ""),
        (1, "application", 50, "application"),
        (51, "website", 50, "website"),
        (101, "bookmark", 50, "bookmark"),
        (151, "window", 50, "window"),
        (201, "calculator", 50, "calculator"),
        (251, "control_panel", 50, "control_panel"),
        (301, "settings", 50, "settings"),
        (351, "document", 50, "document"),
        (401, "command", 50, ""),
        (451, "suggestions", 49, ""),
    ];
    let mut active_group = 0usize;
    let grouped_rows = Rc::new(VecModel::from(
        (0..500)
            .map(|index| {
                if active_group + 1 < groups.len() && index >= groups[active_group + 1].0 {
                    active_group += 1;
                }
                let (start, section_kind, section_count, filter_key) = groups[active_group];
                let is_header = index == start;
                let row_headers_before = active_group as i32 + i32::from(!is_header);
                let category_kind = match section_kind {
                    "bookmark" => "bookmark",
                    "window" => "window",
                    _ => "",
                };
                ResultRow {
                    id: format!("grouped-{index}").into(),
                    title: format!("{section_kind} result with a long title {index}").into(),
                    location: format!("Localized subtitle and source {index}").into(),
                    icon_kind: if index % 2 == 0 { "calculator" } else { "" }.into(),
                    category_kind: category_kind.into(),
                    shortcut_prefix: if section_kind == "command" && index == start {
                        "<"
                    } else {
                        ""
                    }
                    .into(),
                    shortcut_prefix_width: 44,
                    presentation: if index % 2 == 0 {
                        "compact"
                    } else {
                        "detailed"
                    }
                    .into(),
                    section_kind: if is_header { section_kind } else { "" }.into(),
                    section_count,
                    section_more_key: if is_header { filter_key } else { "" }.into(),
                    headers_before: row_headers_before,
                    is_best_match: index == 0,
                    ..Default::default()
                }
            })
            .collect::<Vec<_>>(),
    ));

    // Grouped list: one exhaustive selection sweep at the baseline, then
    // sparse boundary samples for the other language and fractional DPI.
    // Repeating the full traversal for every language/scale pair renders the
    // same layout paths without adding coverage.
    let count = MAX_RENDERED_RESULTS;
    window.set_category_filters(ModelRc::from(Rc::new(VecModel::from(
        Vec::<CategoryFilter>::new(),
    ))));
    let rows = Rc::new(VecModel::from(
        (0..count)
            .filter_map(|index| grouped_rows.row_data(index))
            .collect::<Vec<_>>(),
    ));
    for (language, scale, exhaustive) in [
        ("en", 1.0_f32, true),
        ("zh-CN", 1.0, false),
        ("en", 1.25, false),
        ("zh-CN", 1.25, false),
        ("en", 1.5, false),
        ("zh-CN", 1.5, false),
    ] {
        slint::select_bundled_translation(language).unwrap();
        window.set_accent_background(slint::Color::from_rgb_u8(229, 0, 200));
        // Exercise compact and detailed row heights across the DPI cases.
        window.set_list_item_height(if scale == 1.25 { 35.0 } else { 65.0 });
        window
            .window()
            .dispatch_event(slint::platform::WindowEvent::ScaleFactorChanged {
                scale_factor: scale,
            });
        let width = (520.0 * scale).round() as u32;
        let height = (600.0 * scale).round() as u32;
        diagnostics.borrow_mut().clear();
        native.set_size(slint::PhysicalSize::new(width, height));
        window.set_results(ModelRc::from(rows.clone()));
        window.invoke_reset_result_scroll();
        window.show().unwrap();
        // Both ends, the middle and both sides of each section
        // boundary; pixel coverage must include the entire selected row.
        let context = format!("count={count}, language={language}, scale={scale}");
        for selected in grouped_selections(&groups, count, exhaustive) {
            assert_selected_row_visible(
                &window,
                &native,
                &diagnostics,
                width,
                height,
                scale,
                selected,
                &context,
            );
        }
        window.hide().unwrap();
        if std::env::var_os("WOWIE_LAYOUT_PREVIEW").is_some()
            && count == 256
            && language == "en"
            && scale == 1.0
        {
            let preview_groups = [
                ("best_match", 1, ""),
                ("application", 3, "application"),
                ("window", 3, "window"),
                ("bookmark", 3, "bookmark"),
                ("other", 3, "other"),
            ];
            let mut preview_headers_before = 0;
            let mut preview_rows = Vec::new();
            for (group_index, (kind, group_count, more_key)) in preview_groups.iter().enumerate() {
                if group_index > 0 {
                    preview_headers_before += 1;
                }
                for item_index in 0..*group_count {
                    let is_header = item_index == 0;
                    preview_rows.push(ResultRow {
                        id: format!("preview-{kind}-{item_index}").into(),
                        title: format!("{kind} example {item_index}").into(),
                        location: format!("Source and location for {kind}").into(),
                        icon_kind: if *kind == "window" {
                            "window"
                        } else {
                            "calculator"
                        }
                        .into(),
                        category_kind: match *kind {
                            "window" => "window",
                            "bookmark" => "bookmark",
                            _ => "",
                        }
                        .into(),
                        section_kind: if is_header { *kind } else { "" }.into(),
                        section_count: *group_count,
                        section_more_key: if is_header { *more_key } else { "" }.into(),
                        headers_before: preview_headers_before + i32::from(!is_header),
                        is_best_match: group_index == 0,
                        ..Default::default()
                    });
                }
            }
            window.set_results(ModelRc::from(Rc::new(VecModel::from(preview_rows))));
            window.set_accent_background(slint::Color::from_argb_u8(0x28, 0x00, 0x67, 0xc0));
            window.invoke_reset_result_scroll();
            window.set_selected_index(0);
            window.set_ui_font_size(14.0);
            window
                .window()
                .dispatch_event(slint::platform::WindowEvent::ScaleFactorChanged {
                    scale_factor: 1.0,
                });
            let preview_width = 520;
            let preview_height = 600;
            native.set_size(slint::PhysicalSize::new(preview_width, preview_height));
            let mut pixels =
                vec![slint::Rgb8Pixel::default(); preview_width as usize * preview_height as usize];
            window.show().unwrap();
            window.window().request_redraw();
            native.draw_if_needed(|renderer| {
                renderer.render(&mut pixels, preview_width as usize);
            });
            let rgba = pixels
                .iter()
                .flat_map(|pixel| [pixel.r, pixel.g, pixel.b, 255])
                .collect::<Vec<_>>();
            let preview = std::path::Path::new("target/previews/launcher-grouped-results.png");
            std::fs::create_dir_all(preview.parent().unwrap()).unwrap();
            image::RgbaImage::from_raw(preview_width, preview_height, rgba)
                .unwrap()
                .save(preview)
                .unwrap();
            window.set_list_item_height(50.0);
            window.set_query_text("example".into());
            window.set_root_query_empty(false);
            window.set_query_results_ready(true);
            window.set_search_feedback_visible(false);
            for (dark, theme_name) in [(false, "light"), (true, "dark")] {
                window.set_dark_mode(dark);
                window.set_accent_color(if dark {
                    slint::Color::from_rgb_u8(148, 146, 220)
                } else {
                    slint::Color::from_rgb_u8(98, 96, 168)
                });
                window.set_accent_background(if dark {
                    slint::Color::from_argb_u8(0x42, 148, 146, 220)
                } else {
                    slint::Color::from_argb_u8(0x28, 98, 96, 168)
                });
                window.window().request_redraw();
                save_current_software_preview(
                    &native,
                    &format!("launcher-final-{theme_name}.png"),
                    preview_width,
                    preview_height,
                );
                window.set_query_results_ready(false);
                window.set_search_feedback_visible(true);
                window.window().request_redraw();
                save_current_software_preview(
                    &native,
                    &format!("launcher-pending-{theme_name}.png"),
                    preview_width,
                    preview_height,
                );
                window.set_query_results_ready(true);
                window.set_search_feedback_visible(false);
            }
            window.hide().unwrap();
            window.set_list_item_height(65.0);
            window.set_accent_color(slint::Color::from_rgb_u8(0, 103, 192));
            window.set_accent_background(slint::Color::from_rgb_u8(229, 0, 200));

            let settings_window = SettingsWindow::new().unwrap();
            let settings_config = crate::config::Config::default();
            settings_window.set_native_window_frame(false);
            settings_window.set_dark_mode(false);
            let (settings_accent, _) = accent_colors(&settings_config, 0x0067c0, false);
            settings_window.set_accent_color(settings_accent);
            settings_window.set_categories(settings::categories("en", &[]));
            settings_window.set_selected_category("appearance".into());
            settings_window.set_selected_category_title("Appearance".into());
            settings_window.set_entries(settings::entries(
                &settings_config,
                &[],
                "appearance",
                "en",
            ));
            let settings_native = software_windows.borrow()[1].clone();
            settings_native.set_size(slint::PhysicalSize::new(900, 650));
            settings_window.show().unwrap();
            settings_window.window().request_redraw();
            save_current_software_preview(
                &settings_native,
                "settings-appearance-light.png",
                900,
                650,
            );
            settings_window.hide().unwrap();
            window.set_results(ModelRc::from(rows.clone()));
            window.set_accent_background(slint::Color::from_rgb_u8(229, 0, 200));
            window.set_ui_font_size(20.0);
            window.hide().unwrap();
        }
    }

    // Detail layout is a direct geometry state, not an animated column squeeze.
    window.set_native_window_frame(true);
    window.set_animations_enabled(true);
    window.set_list_item_height(50.0);
    window.set_results(ModelRc::from(rows.clone()));
    window.set_selected_index(0);
    window.set_detail_panel_visible(false);
    window.set_detail_panel_preferred_width(0);
    window.set_accent_background(slint::Color::from_rgb_u8(229, 0, 200));
    window
        .window()
        .dispatch_event(slint::platform::WindowEvent::ScaleFactorChanged { scale_factor: 1.0 });
    native.set_size(slint::PhysicalSize::new(520, 600));
    window.show().unwrap();
    window.window().request_redraw();
    let full_list_right = selected_highlight_right_edge(&native, 520, 600)
        .expect("selected row highlight should render before opening details");
    window.set_detail_panel_preferred_width(240);
    window.set_detail_panel_visible(true);
    window.window().request_redraw();
    let detail_list_right = selected_highlight_right_edge(&native, 520, 600)
        .expect("selected row highlight should remain visible beside details");
    assert!(
        detail_list_right + 80 < full_list_right,
        "opening details should immediately reserve the detail column: full={full_list_right}, detail={detail_list_right}"
    );
    window.set_detail_panel_visible(false);
    window.set_detail_panel_preferred_width(0);
    window.set_selected_index(-1);
    window.hide().unwrap();

    // Sections that only appear deep in a long virtualized list are outside
    // the 256-row surface limit; keep one sparse pass to cover their headers.
    let deep_count = 500usize;
    window.set_category_filters(ModelRc::from(Rc::new(VecModel::from(vec![
        CategoryFilter {
            key: "all".into(),
            count: 500,
        },
        CategoryFilter {
            key: "command".into(),
            count: 50,
        },
        CategoryFilter {
            key: "other".into(),
            count: 450,
        },
    ]))));
    let deep_rows = Rc::new(VecModel::from(
        (0..deep_count)
            .filter_map(|index| grouped_rows.row_data(index))
            .collect::<Vec<_>>(),
    ));
    slint::select_bundled_translation("en").unwrap();
    window.set_accent_background(slint::Color::from_rgb_u8(229, 0, 200));
    window.set_list_item_height(65.0);
    window
        .window()
        .dispatch_event(slint::platform::WindowEvent::ScaleFactorChanged { scale_factor: 1.0 });
    let width = 520;
    let height = 600;
    diagnostics.borrow_mut().clear();
    native.set_size(slint::PhysicalSize::new(width, height));
    window.set_results(ModelRc::from(deep_rows));
    window.invoke_reset_result_scroll();
    window.show().unwrap();
    let context = "count=500, language=en, scale=1.0";
    for selected in grouped_selections(&groups[7..], deep_count, false) {
        assert_selected_row_visible(
            &window,
            &native,
            &diagnostics,
            width,
            height,
            1.0,
            selected,
            context,
        );
    }
    window.hide().unwrap();
}
