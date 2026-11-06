use super::*;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType, Rgb565Pixel};
use slint::platform::{Platform, PlatformError, WindowAdapter};

struct LayoutPlatform {
    window: Rc<MinimalSoftwareWindow>,
    diagnostics: Rc<std::cell::RefCell<Vec<String>>>,
}

impl Platform for LayoutPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.window.clone())
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

#[test]
fn launcher_layout_settles_when_reopened() {
    // Use the actual compiled UI and collect runtime diagnostics without
    // opening native windows or depending on a GPU. The zero-height viewport
    // regression reproduces when a scrolled list changes size in compact mode.
    let native = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    let diagnostics = Rc::new(std::cell::RefCell::new(Vec::new()));
    slint::platform::set_platform(Box::new(LayoutPlatform {
        window: native.clone(),
        diagnostics: diagnostics.clone(),
    }))
    .unwrap();
    let window = Launcher::new().unwrap();
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
            window.set_results(ModelRc::from(rows.clone()));
            window.set_accent_background(slint::Color::from_rgb_u8(229, 0, 200));
            window.set_ui_font_size(20.0);
            window.hide().unwrap();
        }
    }

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
