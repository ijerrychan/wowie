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
        (1.25, 49.0, true, 3),
        (1.25, 50.0, true, 8),
        (1.5, 35.0, false, 8),
        (1.75, 50.0, true, 100),
        (2.0, 65.0, true, 256),
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
            for height in [350, 56, 600, 350] {
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
}
